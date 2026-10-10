//! Remote delegation over the real service: the strictly-lower hierarchy, the exact
//! delegation capabilities, custom roles, per-principal overrides and device
//! revocation.
//!
//! Java reference methods translated here:
//!   * `remoteDelegationIsStrictlyLowerAndCannotCreateAdminOrReservedCustomRoles`
//!   * `deviceRevocationHonorsOwnerDenyAndStrictHierarchyWithoutSelfBypass`
//!
//! Every assertion runs against real SQLite state, real transactions, real audit
//! rows and the fixture clock. No service, audit log, database or local authority is
//! mocked, and no production seam is added.
use super::support::{Fixture, fingerprint};
use crate::identity::{
    AssignmentId, AuthorityChange, BuiltinRole, Capability, CustomRank, CustomRole, Decision,
    DenyReason, DeviceId, Effect, Expiry, OverrideId, PolicySnapshot, PrincipalId, RemoteCommand,
    Resource, ResourceScope, RoleGrant, RoleId, RoleName, Subject,
};
use crate::{ControlError, IdentityErrorCode};
use vesta_state::Clock;

// ---------------------------------------------------------------- shared helpers

fn builtin(role: BuiltinRole) -> RoleId {
    RoleId::from(role)
}

fn scope(text: &str) -> ResourceScope {
    ResourceScope::parse(text).unwrap_or_else(|_| panic!("{text:?} must be a canonical scope"))
}

fn resource(text: &str) -> Resource {
    Resource::parse(text).unwrap_or_else(|_| panic!("{text:?} must be a canonical resource"))
}

fn grant(capability: Capability, text: &str) -> RoleGrant {
    RoleGrant::new(capability, scope(text)).expect("canonical role grant")
}

/// A `check` in the only shape the capability accepts: a scoped capability needs an
/// explicit resource (ROOT is valid), an unscoped one needs `None`.
fn allowed(policy: &PolicySnapshot, capability: Capability) -> bool {
    policy
        .check(capability, capability.scoped().then_some(&Resource::ROOT))
        .allowed()
}

fn remote(
    fixture: &Fixture,
    actor: &Subject,
    command: RemoteCommand,
) -> crate::Result<AuthorityChange> {
    fixture.identities.apply_remote(actor, command)
}

fn snapshot_of(fixture: &Fixture, subject: &Subject) -> PolicySnapshot {
    fixture
        .identities
        .current_policy(Some(subject))
        .expect("current policy is observable")
}

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

/// Unrevoked primary-role rows for one principal, expired or not.
fn active_role_ids(fixture: &Fixture, principal: &PrincipalId) -> Vec<String> {
    text_column(
        fixture,
        &format!(
            "SELECT role_id FROM role_assignments WHERE principal_id = '{}' AND revoked_at IS NULL ORDER BY role_id",
            principal.as_str()
        ),
    )
}

fn live_assignment(fixture: &Fixture, principal: &PrincipalId) -> AssignmentId {
    let id: String = scalar(
        fixture,
        &format!(
            "SELECT id FROM role_assignments WHERE principal_id = '{}' AND revoked_at IS NULL \
             ORDER BY created_at DESC, id DESC LIMIT 1",
            principal.as_str()
        ),
    );
    AssignmentId::parse(&id).expect("stored assignment id")
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

fn assert_last_audit(fixture: &Fixture, action: &str, outcome: &str) {
    let entries = fixture.audit.page(0, 500).expect("audit page");
    let last = entries
        .last()
        .unwrap_or_else(|| panic!("{action}: nothing was audited"));
    assert_eq!(last.action, action, "audited action");
    assert_eq!(last.outcome, outcome, "audited outcome for {action}");
}

/// A refusal is `"denied"` for `forbidden` and its only metadata key is `reason`.
fn assert_last_refusal(fixture: &Fixture, action: &str, reason: DenyReason) {
    assert_last_audit(fixture, action, "denied");
    let entries = fixture.audit.page(0, 500).expect("audit page");
    let last = entries.last().expect("a refusal row");
    assert_eq!(
        last.metadata_json,
        format!("{{\"reason\":\"{}\"}}", reason.code()),
        "refusal metadata for {action}"
    );
}

/// A clean domain rejection is `"rejected"`, with the fixed error code as its reason.
fn assert_last_rejection(fixture: &Fixture, action: &str, code: IdentityErrorCode) {
    assert_last_audit(fixture, action, "rejected");
    let entries = fixture.audit.page(0, 500).expect("audit page");
    let last = entries.last().expect("a rejection row");
    assert_eq!(
        last.metadata_json,
        format!("{{\"reason\":\"{}\"}}", code.code()),
        "rejection metadata for {action}"
    );
}

fn assert_success_audit(fixture: &Fixture, action: &str) {
    assert_last_audit(fixture, action, "success");
    let entries = fixture.audit.page(0, 500).expect("audit page");
    let last = entries.last().expect("a success row");
    assert!(
        !last.metadata_json.contains("reason"),
        "{action}: a success audit never records a reason"
    );
}

fn assert_denied(result: crate::Result<AuthorityChange>, reason: DenyReason, label: &str) {
    match result {
        Ok(change) => panic!(
            "{label}: expected forbidden/{} but got {change:?}",
            reason.code()
        ),
        Err(error) => {
            assert_eq!(error.code(), "forbidden", "{label}: error code");
            assert_eq!(error.reason(), Some(reason), "{label}: deny reason");
        }
    }
}

/// A domain rejection is not a decision: a fixed code and no deny reason at all.
fn assert_rejected(result: crate::Result<AuthorityChange>, code: IdentityErrorCode, label: &str) {
    match result {
        Ok(change) => panic!("{label}: expected {} but got {change:?}", code.code()),
        Err(error) => {
            assert_eq!(error.code(), code.code(), "{label}: error code");
            assert_eq!(
                error.reason(),
                None,
                "{label}: a rejection is not a decision"
            );
        }
    }
}

fn expect_assignment(result: crate::Result<AuthorityChange>, label: &str) -> AssignmentId {
    match result.unwrap_or_else(|error| panic!("{label}: {error:?}")) {
        AuthorityChange::Assignment(id) => id,
        other => panic!("{label}: expected an assignment, got {other:?}"),
    }
}

fn expect_override(result: crate::Result<AuthorityChange>, label: &str) -> OverrideId {
    match result.unwrap_or_else(|error| panic!("{label}: {error:?}")) {
        AuthorityChange::Override(id) => id,
        other => panic!("{label}: expected an override, got {other:?}"),
    }
}

fn expect_role(result: crate::Result<AuthorityChange>, label: &str) -> RoleId {
    match result.unwrap_or_else(|error| panic!("{label}: {error:?}")) {
        AuthorityChange::Role(id) => id,
        other => panic!("{label}: expected a role, got {other:?}"),
    }
}

fn expect_revoked(result: crate::Result<AuthorityChange>, label: &str) {
    assert_eq!(
        result.unwrap_or_else(|error| panic!("{label}: {error:?}")),
        AuthorityChange::Revoked,
        "{label}"
    );
}

/// Creates a custom role through the real `RemoteCommand::CreateRole` command, so the
/// name-uniqueness barrier, the actor's containment check and the success audit all
/// run. Used as a fixture builder for the sub-tier delegator roles below.
fn custom_role(
    fixture: &Fixture,
    owner: &Subject,
    name: &str,
    rank: i32,
    grants: Vec<RoleGrant>,
) -> RoleId {
    expect_role(
        remote(
            fixture,
            owner,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse(name).expect("role name"),
                    CustomRank::new(rank).expect("custom rank"),
                    grants,
                )
                .expect("custom role value"),
            ),
        ),
        &format!("owner creates {name:?}"),
    )
}

/// A fresh fixture principal holding `role`, assigned through the real local
/// `LocalCommand::AssignRole` path.
fn holder(fixture: &Fixture, role: RoleId) -> Subject {
    fixture.subject(role)
}

/// A delegator custom role plus its holder. `roles.assign`, `roles.create`,
/// `roles.revoke` and `devices.revoke` are in no low built-in preset, so every
/// sub-tier actor in this file is necessarily a custom role.
fn delegator(
    fixture: &Fixture,
    owner: &Subject,
    name: &str,
    rank: i32,
    grants: Vec<RoleGrant>,
) -> Subject {
    let role = custom_role(fixture, owner, name, rank, grants);
    holder(fixture, role)
}

/// Plants a stored custom-role row plus whole-scope grants. `SetOverride` cannot
/// express "every capability except one", and `CreateRole` cannot carry a reserved
/// capability, so a few principals must exist as real rows.
fn plant_role_row(fixture: &Fixture, name: &str, rank: i32, capabilities: &[Capability]) -> RoleId {
    let id = RoleId::parse(&format!("planted-{}", uuid::Uuid::new_v4())).expect("planted role id");
    let mut script = format!(
        "INSERT INTO roles (id, name, rank, builtin, created_by, created_at) \
         VALUES ('{}','{name}',{rank},0,'fixture',{});",
        id.as_str(),
        fixture.clock.millis(),
    );
    for capability in capabilities {
        script.push_str(&format!(
            "INSERT INTO role_capabilities (role_id, capability, scope) VALUES ('{}','{}','*');",
            id.as_str(),
            capability.id(),
        ));
    }
    fixture.sql(&script);
    assert_eq!(
        scalar::<i64>(
            fixture,
            &format!(
                "SELECT count(*) FROM role_capabilities WHERE role_id = '{}'",
                id.as_str()
            )
        ),
        capabilities.len() as i64,
        "every planted grant row must exist"
    );
    id
}

/// A second device for an existing principal, so "the principal's remaining
/// authority" stays observable after one device is revoked.
fn plant_device(fixture: &Fixture, subject: &Subject, label: &str) -> DeviceId {
    let id = uuid::Uuid::new_v4().to_string();
    fixture.sql(&format!(
        "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) \
         VALUES ('{id}','{}','{}','{label}','{}',{});",
        fixture.db.server_id(),
        subject.principal.as_str(),
        fingerprint(&id).as_str(),
        fixture.clock.millis(),
    ));
    DeviceId::parse(&id).expect("planted device id")
}

/// Plants the imported row the Java fixture plants: a device bound to a server id
/// that is not this server. Every identity table references `server_identity`, so the
/// row is only writable by a connection that does not enforce references â€” the same
/// external-import path `tests/policy.rs` uses. Only `devices.server_id` is rewritten.
fn plant_foreign_server_device(fixture: &Fixture, subject: &Subject) {
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
        .execute(
            "UPDATE devices SET server_id = 'foreign-server' WHERE id = ?1",
            [subject.device.as_str()],
        )
        .expect("plant a foreign device server id");
}

/// Every observable authority statement about a subject must be "nothing at all".
fn assert_resolves_no_authority(fixture: &Fixture, subject: Option<&Subject>, label: &str) {
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
    for role in BuiltinRole::ALL {
        assert!(!policy.holds_builtin(*role), "{label}: {role:?}");
    }
    for capability in Capability::ALL {
        assert!(
            !allowed(&policy, *capability),
            "{label}: {}",
            capability.id()
        );
        assert!(
            !policy.holds_entirely(*capability, &ResourceScope::ALL),
            "{label}: holds_entirely {}",
            capability.id()
        );
    }
}

/// A principal whose only role was revoked keeps its identity but has no authority at
/// all: a live device is not a delegation capability.
fn assert_no_authority_at_all(fixture: &Fixture, subject: &Subject, label: &str) {
    let policy = snapshot_of(fixture, subject);
    assert!(
        policy.has_current_identity(),
        "{label}: the device is still live"
    );
    assert_eq!(policy.rank(), 0, "{label}: rank");
    assert!(policy.roles().is_empty(), "{label}: roles");
    for tier in BuiltinRole::ALL {
        assert!(!policy.holds_builtin(*tier), "{label}: {tier:?}");
    }
    for capability in Capability::ALL {
        assert!(
            !allowed(&policy, *capability),
            "{label}: {}",
            capability.id()
        );
    }
}

// =================================================== A: strictly-lower delegation

/// Java: `remoteDelegationIsStrictlyLowerAndCannotCreateAdminOrReservedCustomRoles`
#[test]
fn an_owner_assigns_every_strictly_lower_builtin_but_never_a_locally_granted_tier() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    let maintainer = fixture.subject(builtin(BuiltinRole::Maintainer));
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let pack_user = fixture.subject(builtin(BuiltinRole::PackUser));

    // Every built-in tier strictly below Owner is delegable, and each assignment
    // replaces the principal's single primary role.
    for role in [
        BuiltinRole::Maintainer,
        BuiltinRole::Moderator,
        BuiltinRole::Viewer,
        BuiltinRole::PackUser,
    ] {
        expect_assignment(
            remote(
                &fixture,
                &owner,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: builtin(role),
                    expiry: Expiry::Never,
                },
            ),
            &format!("owner assigns {role:?}"),
        );
        assert_eq!(
            snapshot_of(&fixture, &viewer).rank(),
            role.rank(),
            "{role:?} must be the effective rank"
        );
        assert_eq!(
            active_role_ids(&fixture, &viewer.principal),
            vec![builtin(role).as_str().to_owned()]
        );
    }

    // Owner and Administrator are reachable only through the local-proof path. The
    // `local_grant_only` branch runs before any rank comparison, so even the Owner
    // reports `local_grant_only` for its own tier and never `rank`.
    for local_only in [BuiltinRole::Owner, BuiltinRole::Administrator] {
        assert_denied(
            remote(
                &fixture,
                &owner,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: builtin(local_only),
                    expiry: Expiry::Never,
                },
            ),
            DenyReason::LocalGrantOnly,
            &format!("owner assigns {local_only:?}"),
        );
    }

    // Java: an Administrator cannot mint an Owner either â€¦
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: builtin(BuiltinRole::Owner),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::LocalGrantOnly,
        "administrator assigns owner",
    );
    // â€¦ and cannot act on a principal of equal or higher rank.
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::AssignRole {
                principal: owner.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::Rank,
        "administrator targets the owner",
    );
    // Java: the Maintainer preset carries no delegation capability at all, so the
    // refusal is `not_granted`, not a complaint about rank.
    assert_denied(
        remote(
            &fixture,
            &maintainer,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotGranted,
        "maintainer without roles.assign",
    );
    // Java: an Administrator may still assign Maintainer below itself.
    expect_assignment(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: builtin(BuiltinRole::Maintainer),
                expiry: Expiry::Never,
            },
        ),
        "administrator assigns maintainer",
    );
    assert_eq!(
        snapshot_of(&fixture, &viewer).rank(),
        BuiltinRole::Maintainer.rank()
    );

    // Nothing reserved leaked: no live Owner/Administrator assignment anywhere.
    assert_eq!(
        scalar::<i64>(
            &fixture,
            &format!(
                "SELECT count(*) FROM role_assignments a JOIN roles r ON r.id = a.role_id \
                 WHERE a.principal_id = '{}' AND a.revoked_at IS NULL AND r.builtin = 1 \
                 AND r.id IN ('owner','administrator')",
                viewer.principal.as_str()
            )
        ),
        0,
        "a locally granted tier must never appear remotely"
    );
    assert!(fixture.identities.owner_exists().expect("owner exists"));
    assert!(snapshot_of(&fixture, &owner).has_current_identity());

    // A role of the delegator's own rank is refused by the role branch, which runs
    // before the target branch. Unreachable for an Owner or an Administrator through
    // the public API (`CustomRank` caps a custom role at 79 and both tiers are 80+),
    // so it is exercised below with a custom-role delegator instead.
    let rank_fifty = custom_role(
        &fixture,
        &owner,
        "Rank fifty",
        50,
        vec![grant(Capability::StatusView, "*")],
    );
    let peer = delegator(
        &fixture,
        &owner,
        "Fifty delegator",
        50,
        vec![grant(Capability::RolesAssign, "*")],
    );
    assert_denied(
        remote(
            &fixture,
            &peer,
            RemoteCommand::AssignRole {
                principal: peer.principal.clone(),
                role: rank_fifty.clone(),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::Rank,
        "a role of exactly the delegator's rank",
    );

    // Lookups are resolved before authorization, so an unknown principal or role is
    // `not_found` rather than a decision about authority.
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::AssignRole {
                principal: PrincipalId::parse("no-such-principal").expect("principal id"),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        IdentityErrorCode::NotFound,
        "unknown principal",
    );
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: RoleId::parse("no-such-role").expect("role id"),
                expiry: Expiry::Never,
            },
        ),
        IdentityErrorCode::NotFound,
        "unknown role",
    );
    assert_last_rejection(
        &fixture,
        "authority.role.assign",
        IdentityErrorCode::NotFound,
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

/// Independent negative: no actor, at any rank, may name itself as the target.
#[test]
fn a_self_target_is_refused_before_any_rank_comparison() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let lower = fixture.subject(builtin(BuiltinRole::PackUser));

    // Both a far lower role and a rank-adjacent role report `self`: `lower_target`
    // compares identity before rank.
    for role in [BuiltinRole::PackUser, BuiltinRole::Maintainer] {
        assert_denied(
            remote(
                &fixture,
                &owner,
                RemoteCommand::AssignRole {
                    principal: owner.principal.clone(),
                    role: builtin(role),
                    expiry: Expiry::Never,
                },
            ),
            DenyReason::SelfTarget,
            &format!("owner assigns {role:?} to itself"),
        );
    }
    assert_denied(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: owner.principal.clone(),
                grant: grant(Capability::FilesRead, "config/**"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::SelfTarget,
        "owner overrides itself",
    );
    assert_eq!(active_role_ids(&fixture, &owner.principal).len(), 1);

    // A sub-tier delegator with a real `roles.assign` and a role it may hand out.
    let actor = delegator(
        &fixture,
        &owner,
        "Self delegator",
        50,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::StatusView, "*"),
        ],
    );
    let peer_role = custom_role(
        &fixture,
        &owner,
        "Peer fifty",
        50,
        vec![grant(Capability::StatusView, "*")],
    );
    let low_role = custom_role(
        &fixture,
        &owner,
        "Low reader",
        20,
        vec![grant(Capability::StatusView, "*")],
    );
    let peer = holder(&fixture, peer_role.clone());

    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: actor.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::SelfTarget,
        "a sub-tier self target, role strictly lower",
    );
    // A role of the delegator's own rank is refused before the self check even runs.
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: actor.principal.clone(),
                role: peer_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::Rank,
        "a role of the delegator's own rank wins over self",
    );
    // An equal-rank *target* is a hierarchy refusal, not a self refusal.
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: peer.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::Rank,
        "an equal-rank target",
    );
    // The strictly lower target still works, so nothing above is a fixture fault.
    expect_assignment(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: lower.principal.clone(),
                role: low_role,
                expiry: Expiry::Never,
            },
        ),
        "a sub-tier delegator delegates",
    );
    assert_eq!(snapshot_of(&fixture, &lower).rank(), 20);
    assert!(allowed(
        &snapshot_of(&fixture, &lower),
        Capability::StatusView
    ));
}

/// Independent negative: `roles.assign` is required exactly, and holding every other
/// capability is not a substitute.
#[test]
fn delegation_requires_the_exact_roles_assign_capability() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let pack_user = fixture.subject(builtin(BuiltinRole::PackUser));

    // A principal that holds every capability except `roles.assign` exists as a real
    // stored role row, because `SetOverride` cannot express "all but one".
    let everything_else = Capability::ALL
        .iter()
        .copied()
        .filter(|capability| *capability != Capability::RolesAssign)
        .collect::<Vec<_>>();
    assert_eq!(everything_else.len(), Capability::ALL.len() - 1);
    let nearly_admin = plant_role_row(&fixture, "Nearly everything", 79, &everything_else);
    let actor = holder(&fixture, nearly_admin);

    // It still holds `roles.create`, `roles.revoke` and `devices.revoke`, so those
    // three commands are the positive controls for their own exactness: a second
    // principal without ANY delegation capability is what refuses them.
    let no_delegation_caps = Capability::ALL
        .iter()
        .copied()
        .filter(|capability| {
            !matches!(
                capability,
                Capability::RolesAssign
                    | Capability::RolesRevoke
                    | Capability::RolesCreate
                    | Capability::DevicesRevoke
            )
        })
        .collect::<Vec<_>>();
    let bare = holder(
        &fixture,
        plant_role_row(&fixture, "Without delegation", 79, &no_delegation_caps),
    );

    let policy = snapshot_of(&fixture, &actor);
    assert_eq!(policy.rank(), 79, "the highest legal custom rank");
    for tier in BuiltinRole::ALL {
        assert!(
            !policy.holds_builtin(*tier),
            "a custom role confers no built-in tier ({tier:?})"
        );
    }
    // The exact missing capability, and nothing else that is delegable.
    assert_eq!(
        policy.check(Capability::RolesAssign, None),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert_eq!(
        policy.check(Capability::RolesAssign, None).reason(),
        "not_granted"
    );
    let effective = Capability::ALL
        .iter()
        .copied()
        .filter(|capability| allowed(&policy, *capability))
        .collect::<Vec<_>>();
    assert_eq!(
        effective,
        Capability::ALL
            .iter()
            .copied()
            .filter(|capability| {
                *capability != Capability::RolesAssign && capability.delegable()
            })
            .collect::<Vec<_>>(),
        "exactly the delegable capabilities minus roles.assign are effective"
    );
    // Reserved capabilities in a stored custom row stay reserved without the tier.
    for reserved in [
        Capability::BackupsRestore,
        Capability::RecoveryExport,
        Capability::CredentialsManage,
        Capability::SecurityManage,
    ] {
        assert_eq!(
            policy.check(reserved, None),
            Decision::Deny(DenyReason::Reserved),
            "{} must not become effective",
            reserved.id()
        );
    }
    for local in [Capability::OwnerRecovery, Capability::AgentInstall] {
        assert_eq!(
            policy.check(local, None),
            Decision::Deny(DenyReason::LocalOnly),
            "{} is local only",
            local.id()
        );
    }

    // Every command refuses, each for its own exact capability. Every target is a live
    // row, because the lookup runs before the capability check. `AssignRole` and both
    // override effects need `roles.assign`, which `actor` lacks; the three revocation
    // and creation commands need their own capability, which `bare` lacks.
    let pack_user_assignment = expect_assignment(
        remote(
            &fixture,
            &owner,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        "owner assigns viewer",
    );
    let viewer_override = OverrideId::parse(&fixture.stored_override(
        &viewer,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        None,
    ))
    .expect("stored override id");
    let before = authority_counts(&fixture);
    let missing_assign = [
        (
            "assign",
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        (
            "override allow",
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: grant(Capability::FilesRead, "config/**"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        (
            "override deny",
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: grant(Capability::FilesRead, "config/**"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
    ];
    for (label, command) in missing_assign {
        assert_denied(
            remote(&fixture, &actor, command),
            DenyReason::NotGranted,
            label,
        );
    }
    let missing_own = [
        (
            "revoke assignment",
            RemoteCommand::RevokeAssignment(pack_user_assignment),
        ),
        (
            "revoke override",
            RemoteCommand::RevokeOverride(viewer_override),
        ),
        (
            "revoke device",
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        (
            "create role",
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Not mine").expect("role name"),
                    CustomRank::new(15).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
    ];
    for (label, command) in missing_own {
        assert_denied(
            remote(&fixture, &bare, command),
            DenyReason::NotGranted,
            label,
        );
    }
    assert_eq!(
        authority_counts(&fixture),
        before,
        "no refused command mutated anything"
    );
    assert_eq!(active_role_ids(&fixture, &pack_user.principal).len(), 1);
    // `actor` really does hold the three capabilities it was not refused for, which
    // is what makes the four refusals above about the exact capability and nothing
    // else. Each command below touches a fresh live target.
    let spare = fixture.subject(builtin(BuiltinRole::Moderator));
    let second = fixture.subject(builtin(BuiltinRole::Viewer));
    expect_role(
        remote(
            &fixture,
            &actor,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Almost admin").expect("role name"),
                    CustomRank::new(15).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        "roles.create really is held",
    );
    expect_revoked(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeDevice(spare.device.clone()),
        ),
        "devices.revoke really is held",
    );
    expect_revoked(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeAssignment(live_assignment(&fixture, &second.principal)),
        ),
        "roles.revoke really is held",
    );
    assert_denied(
        remote(
            &fixture,
            &bare,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Still not mine").expect("role name"),
                    CustomRank::new(15).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        DenyReason::NotGranted,
        "roles.create is still refused for the bare principal",
    );
    assert_eq!(
        scalar::<i64>(
            &fixture,
            "SELECT count(*) FROM roles WHERE builtin = 0 AND id NOT IN ('public','player')"
        ),
        3,
        "only the two planted rows and the one permitted custom role exist"
    );
    assert!(snapshot_of(&fixture, &viewer).has_current_identity());

    // The positive control: the one missing capability is the whole difference.
    let lending = expect_override(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: actor.principal.clone(),
                grant: grant(Capability::RolesAssign, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        "owner lends roles.assign",
    );
    expect_assignment(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        "the delegator can now delegate",
    );
    expect_revoked(
        remote(&fixture, &owner, RemoteCommand::RevokeOverride(lending)),
        "the owner takes the lent capability back",
    );
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotGranted,
        "and loses it again immediately",
    );
}

/// Independent negative: a denial of one delegation capability does not disturb the
/// others, and an explicit deny beats an inherited allow for every command.
#[test]
fn an_explicit_deny_on_a_delegation_capability_beats_an_inherited_allow() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let pack_user = fixture.subject(builtin(BuiltinRole::PackUser));
    let low_role = custom_role(
        &fixture,
        &owner,
        "Low reader",
        20,
        vec![grant(Capability::StatusView, "*")],
    );
    let actor = delegator(
        &fixture,
        &owner,
        "Fully delegated",
        30,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::RolesCreate, "*"),
            grant(Capability::RolesRevoke, "*"),
            grant(Capability::DevicesRevoke, "*"),
            grant(Capability::StatusView, "*"),
        ],
    );
    // Device targets must be strictly below the rank-30 delegator.
    let victim = fixture.subject(builtin(BuiltinRole::PackUser));
    let spare_one = fixture.subject(builtin(BuiltinRole::PackUser));
    let spare_two = fixture.subject(builtin(BuiltinRole::PackUser));
    let spare_three = fixture.subject(builtin(BuiltinRole::PackUser));
    let pack_user_assignment = live_assignment(&fixture, &pack_user.principal);

    // Baseline: all four delegation capabilities really are effective.
    expect_assignment(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: low_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        "baseline assign",
    );
    expect_revoked(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeDevice(spare_one.device.clone()),
        ),
        "a remote revocation",
    );

    // Deny `roles.assign`: only the two assign-shaped commands are affected.
    fixture.stored_override(&actor, Capability::RolesAssign, "*", Effect::Deny, None);
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: low_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::ExplicitDeny,
        "assign under an explicit deny",
    );
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: grant(Capability::FilesRead, "config/**"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::ExplicitDeny,
        "override under an explicit deny",
    );
    expect_role(
        remote(
            &fixture,
            &actor,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Still allowed").expect("role name"),
                    CustomRank::new(20).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        "roles.create is untouched by a roles.assign deny",
    );
    expect_revoked(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeDevice(spare_two.device.clone()),
        ),
        "a remote revocation",
    );

    // Each further denial is attributable to exactly one capability.
    fixture.stored_override(&actor, Capability::RolesCreate, "*", Effect::Deny, None);
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Now refused").expect("role name"),
                    CustomRank::new(20).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        DenyReason::ExplicitDeny,
        "create role under an explicit deny",
    );
    expect_revoked(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeDevice(spare_three.device.clone()),
        ),
        "a remote revocation",
    );

    fixture.stored_override(&actor, Capability::RolesRevoke, "*", Effect::Deny, None);
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeAssignment(pack_user_assignment),
        ),
        DenyReason::ExplicitDeny,
        "revoke assignment under an explicit deny",
    );
    expect_revoked(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeDevice(victim.device.clone()),
        ),
        "a remote revocation",
    );
    assert_resolves_no_authority(&fixture, Some(&victim), "revoked by a delegate");

    fixture.stored_override(&actor, Capability::DevicesRevoke, "*", Effect::Deny, None);
    let untouched = fixture.subject(builtin(BuiltinRole::PackUser));
    assert_denied(
        remote(
            &fixture,
            &actor,
            RemoteCommand::RevokeDevice(untouched.device.clone()),
        ),
        DenyReason::ExplicitDeny,
        "revoke device under an explicit deny",
    );
    assert!(snapshot_of(&fixture, &untouched).has_current_identity());
    assert!(fixture.identities.owner_exists().expect("owner exists"));
    assert_last_refusal(&fixture, "identity.device.revoke", DenyReason::ExplicitDeny);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

/// Independent negative: a delegated role's grants must be contained in the delegator.
#[test]
fn a_role_may_not_be_delegated_beyond_the_delegators_own_scope() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let pack_user = fixture.subject(builtin(BuiltinRole::PackUser));

    let global_reader = custom_role(
        &fixture,
        &owner,
        "Global reader",
        12,
        vec![grant(Capability::FilesRead, "*")],
    );
    let worlds_reader = custom_role(
        &fixture,
        &owner,
        "Worlds reader",
        13,
        vec![grant(Capability::FilesRead, "worlds/**")],
    );
    // `files.read` on `worlds/**` only, so a whole-scope grant is outside it.
    let narrow = delegator(
        &fixture,
        &owner,
        "Narrow delegator",
        30,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::FilesRead, "worlds/**"),
        ],
    );
    let narrow_policy = snapshot_of(&fixture, &narrow);
    assert!(
        !narrow_policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL),
        "the delegator really holds only the subtree"
    );
    assert!(
        narrow_policy.holds_entirely(Capability::FilesRead, &scope("worlds/**")),
        "and does hold its own scope entirely"
    );
    assert!(
        !narrow_policy.holds_entirely(Capability::FilesRead, &scope("logs/**")),
        "and holds nothing else"
    );

    assert_denied(
        remote(
            &fixture,
            &narrow,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: global_reader.clone(),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotDelegable,
        "a whole-scope role cannot be delegated by a subtree delegator",
    );
    // The very same role is fine from an actor that does hold the whole scope.
    expect_assignment(
        remote(
            &fixture,
            &owner,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: global_reader,
                expiry: Expiry::Never,
            },
        ),
        "owner assigns the whole-scope role",
    );
    assert_eq!(
        snapshot_of(&fixture, &pack_user)
            .check(Capability::FilesRead, Some(&resource("logs/latest.log"))),
        Decision::Allow
    );

    // And the narrow delegator can still delegate exactly what it holds.
    expect_assignment(
        remote(
            &fixture,
            &narrow,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: worlds_reader,
                expiry: Expiry::Never,
            },
        ),
        "the narrow delegator delegates its own scope",
    );
    let policy = snapshot_of(&fixture, &pack_user);
    assert_eq!(
        policy.check(
            Capability::FilesRead,
            Some(&resource("worlds/maps/atlas.png"))
        ),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("logs/latest.log"))),
        Decision::Deny(DenyReason::NotGranted),
        "the primary role was replaced, so the earlier whole-scope grant is gone"
    );

    // Java, translated: the default Maintainer preset has no `players.moderate`, so
    // even once `roles.assign` is in its hands it cannot grant the Moderator role.
    // The refusal is containment (`not_delegable`), not `not_granted` and not `rank`,
    // which is exactly the point Java made with that case.
    let maintainer = fixture.subject(builtin(BuiltinRole::Maintainer));
    expect_override(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: maintainer.principal.clone(),
                grant: grant(Capability::RolesAssign, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        "owner lends roles.assign to a maintainer",
    );
    assert!(
        allowed(&snapshot_of(&fixture, &maintainer), Capability::RolesAssign),
        "the maintainer really does hold roles.assign now"
    );
    assert!(
        !allowed(
            &snapshot_of(&fixture, &maintainer),
            Capability::PlayersModerate
        ),
        "the default maintainer preset has no players.moderate"
    );
    assert_denied(
        remote(
            &fixture,
            &maintainer,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotDelegable,
        "Java: a default maintainer cannot grant beyond its own authority",
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ====================================================== B: custom roles and ranks

#[test]
fn a_custom_rank_is_bounded_below_the_administrator_tier_at_construction() {
    // This is the deliberate stricter replacement for the Java schema CHECK. The
    // bound lives in `CustomRank`, so a rank >= 80 or a rank < 1 is unrepresentable
    // and can never reach `RemoteCommand::CreateRole`, the service or the database;
    // Java relied on `CHECK (rank BETWEEN 1 AND 100)` plus
    // `CHECK (builtin = 1 OR rank < 80)` and only found out on insert.
    let administrator_tier = BuiltinRole::Administrator.rank();
    assert_eq!(administrator_tier, 80);
    assert_eq!(BuiltinRole::Owner.rank(), 100);
    assert!(
        CustomRank::new(administrator_tier - 1).is_ok(),
        "79 is legal"
    );
    assert!(CustomRank::new(1).is_ok(), "1 is the floor");
    for rejected in [0, -1, administrator_tier, 100, i32::MAX] {
        let error: ControlError = CustomRank::new(rejected).expect_err("rank must be rejected");
        assert_eq!(error.code(), "invalid_request", "rank {rejected}");
        assert_eq!(error.reason(), None, "rank {rejected} is not a decision");
    }
    assert_eq!(
        CustomRank::new(administrator_tier - 1)
            .expect("79 is legal")
            .value(),
        79
    );
    // Because the type caps a custom role below 80, no command can ever carry a rank
    // at or above the Owner or Administrator tier.
    assert!(CustomRank::new(79).expect("79 is legal").value() < administrator_tier);
    assert!(CustomRank::new(79).expect("79 is legal").value() < BuiltinRole::Owner.rank());

    // A role needs at least one grant, and no capability may repeat in one role even
    // at different scopes.
    let name = RoleName::parse("Empty").expect("role name");
    let rank = CustomRank::new(15).expect("custom rank");
    assert!(
        CustomRole::new(name.clone(), rank, Vec::new()).is_err(),
        "an empty grant list must be rejected"
    );
    assert!(
        CustomRole::new(
            name.clone(),
            rank,
            vec![
                grant(Capability::FilesRead, "config/**"),
                grant(Capability::FilesRead, "logs/**"),
            ],
        )
        .is_err(),
        "a duplicated capability must be rejected"
    );
    // A readable value keeps the type usable, so the rejections above are the
    // constraints and not a broken fixture.
    let role = CustomRole::new(
        name,
        rank,
        vec![
            grant(Capability::FilesRead, "config/**"),
            grant(Capability::FilesList, "*"),
        ],
    )
    .expect("distinct capabilities are fine");
    assert_eq!(role.grants().len(), 2);
    assert_eq!(role.rank().value(), 15);
    // A non-scoped capability cannot carry a path at all.
    assert!(
        RoleGrant::new(Capability::StatusView, scope("config/**")).is_err(),
        "an unscoped capability must be granted at the whole scope"
    );
    assert!(
        RoleGrant::new(Capability::StatusView, ResourceScope::ALL).is_ok(),
        "and is fine at the whole scope"
    );
}

/// Java: the reserved-capability smuggling loop of
/// `remoteDelegationIsStrictlyLowerAndCannotCreateAdminOrReservedCustomRoles`.
#[test]
fn a_custom_role_may_only_be_created_below_the_actor_and_within_its_authority() {
    let fixture = Fixture::open();
    let owner = fixture.owner();

    // Java: none of the six reserved capabilities may be smuggled into a custom
    // role, not even by the Owner which holds four of them itself. The reserved
    // branches are properties of the capability, not of the delegator.
    let reserved = [
        (Capability::OwnerRecovery, DenyReason::LocalOnly),
        (Capability::AgentInstall, DenyReason::LocalOnly),
        (Capability::BackupsRestore, DenyReason::OwnerReserved),
        (Capability::RecoveryExport, DenyReason::OwnerReserved),
        (Capability::CredentialsManage, DenyReason::AdminReserved),
        (Capability::SecurityManage, DenyReason::AdminReserved),
    ];
    for (capability, reason) in reserved {
        assert!(
            !capability.delegable(),
            "{} must not be delegable",
            capability.id()
        );
        assert_denied(
            remote(
                &fixture,
                &owner,
                RemoteCommand::CreateRole(
                    CustomRole::new(
                        RoleName::parse(&format!("Smuggle {}", capability.id()))
                            .expect("role name"),
                        CustomRank::new(15).expect("custom rank"),
                        vec![grant(capability, "*")],
                    )
                    .expect("custom role value"),
                ),
            ),
            reason,
            &format!("smuggling {}", capability.id()),
        );
    }
    assert_eq!(
        scalar::<i64>(
            &fixture,
            "SELECT count(*) FROM roles WHERE builtin = 0 AND id NOT IN ('public','player')"
        ),
        0,
        "a refused creation must not leave a role or a grant row"
    );
    assert_last_refusal(&fixture, "authority.role.create", DenyReason::AdminReserved);

    // A perfectly delegable role, strictly below the Owner, is accepted.
    custom_role(
        &fixture,
        &owner,
        "Config reader",
        79,
        vec![grant(Capability::FilesRead, "config/**")],
    );
    assert_success_audit(&fixture, "authority.role.create");

    // `roles.create` is required exactly.
    let reader_only = delegator(
        &fixture,
        &owner,
        "Reader only",
        30,
        vec![grant(Capability::FilesRead, "*")],
    );
    assert_denied(
        remote(
            &fixture,
            &reader_only,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Not permitted").expect("role name"),
                    CustomRank::new(20).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        DenyReason::NotGranted,
        "create role without roles.create",
    );

    // Grants must be contained in the creator's own effective authority.
    let scoped_creator = delegator(
        &fixture,
        &owner,
        "Scoped creator",
        30,
        vec![
            grant(Capability::RolesCreate, "*"),
            grant(Capability::FilesRead, "worlds/**"),
        ],
    );
    assert_denied(
        remote(
            &fixture,
            &scoped_creator,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Global leak").expect("role name"),
                    CustomRank::new(20).expect("custom rank"),
                    vec![grant(Capability::FilesRead, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        DenyReason::NotDelegable,
        "creating a whole-scope read role",
    );
    let contained = expect_role(
        remote(
            &fixture,
            &scoped_creator,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Contained reader").expect("role name"),
                    CustomRank::new(20).expect("custom rank"),
                    vec![grant(Capability::FilesRead, "worlds/one")],
                )
                .expect("custom role value"),
            ),
        ),
        "creating a contained read role",
    );
    assert!(contained.as_str().starts_with("custom-"));

    // Strictly lower rank. For an Owner or an Administrator this branch is
    // unreachable through the public API, because `CustomRank` caps a custom role at
    // 79 while both tiers are 80 or above; it is reachable for a custom-role actor,
    // which is what the next block exercises.
    let top_custom = delegator(
        &fixture,
        &owner,
        "Top custom",
        79,
        vec![
            grant(Capability::RolesCreate, "*"),
            grant(Capability::StatusView, "*"),
        ],
    );
    assert_denied(
        remote(
            &fixture,
            &top_custom,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Equal rank").expect("role name"),
                    CustomRank::new(79).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        DenyReason::Rank,
        "a custom role of the actor's own rank",
    );
    let lower_custom = expect_role(
        remote(
            &fixture,
            &top_custom,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("One below").expect("role name"),
                    CustomRank::new(78).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
        "a custom role one rank below the actor",
    );
    assert!(lower_custom.as_str().starts_with("custom-"));
    assert_eq!(snapshot_of(&fixture, &top_custom).rank(), 79);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_duplicate_custom_role_name_is_a_conflict_case_insensitively() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    custom_role(
        &fixture,
        &owner,
        "Config reader",
        15,
        vec![grant(Capability::FilesRead, "config/**")],
    );
    let roles_after_first = scalar::<i64>(&fixture, "SELECT count(*) FROM roles");

    // The name barrier is `name = ? COLLATE NOCASE`, so casing never creates a
    // near-duplicate role.
    for duplicate in ["config reader", "CONFIG READER", "CoNfIg ReAdEr"] {
        assert_rejected(
            remote(
                &fixture,
                &owner,
                RemoteCommand::CreateRole(
                    CustomRole::new(
                        RoleName::parse(duplicate).expect("role name"),
                        CustomRank::new(16).expect("custom rank"),
                        vec![grant(Capability::FilesList, "*")],
                    )
                    .expect("custom role value"),
                ),
            ),
            IdentityErrorCode::Conflict,
            duplicate,
        );
        assert_eq!(
            scalar::<i64>(&fixture, "SELECT count(*) FROM roles"),
            roles_after_first,
            "{duplicate} must not create a role"
        );
    }
    // A custom role may not take a built-in role's display name either.
    for builtin_name in ["Viewer", "VIEWER", "Owner", "Pack User"] {
        assert_rejected(
            remote(
                &fixture,
                &owner,
                RemoteCommand::CreateRole(
                    CustomRole::new(
                        RoleName::parse(builtin_name).expect("role name"),
                        CustomRank::new(16).expect("custom rank"),
                        vec![grant(Capability::StatusView, "*")],
                    )
                    .expect("custom role value"),
                ),
            ),
            IdentityErrorCode::Conflict,
            builtin_name,
        );
    }
    assert_eq!(
        scalar::<i64>(&fixture, "SELECT count(*) FROM roles"),
        roles_after_first
    );
    // A conflict is a rejection, not a denial: the audit outcome and the recorded
    // reason differ accordingly.
    assert_last_rejection(
        &fixture,
        "authority.role.create",
        IdentityErrorCode::Conflict,
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

/// Java: the `Config reader` custom role created by an Administrator and assigned to
/// a Pack User, whose effective read scope is exactly the role's.
#[test]
fn a_created_custom_role_takes_effect_and_its_assignment_stays_contained() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    let pack_user = fixture.subject(builtin(BuiltinRole::PackUser));

    let narrow_role = custom_role(
        &fixture,
        &owner,
        "Config reader",
        15,
        vec![grant(Capability::FilesRead, "config/**")],
    );
    expect_assignment(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: narrow_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        "administrator assigns the custom role",
    );

    let policy = snapshot_of(&fixture, &pack_user);
    assert_eq!(policy.rank(), 15, "the custom rank is the effective rank");
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("config/options.txt"))),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("logs/latest.log"))),
        Decision::Deny(DenyReason::NotGranted),
        "Java: the role does not widen the read scope"
    );
    assert!(
        policy.holds_entirely(Capability::FilesRead, &scope("config/**")),
        "the holder may delegate exactly the role's own scope"
    );
    assert!(
        policy.holds_entirely(Capability::FilesRead, &scope("config/private/**")),
        "a subtree inside the role's subtree is contained"
    );
    assert!(
        !policy.holds_entirely(Capability::FilesRead, &scope("logs/**")),
        "and not a sibling subtree"
    );
    assert!(
        !policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL),
        "and not the whole scope"
    );
    // The replaced Pack User preset is gone, and a custom role confers no tier.
    assert_eq!(
        policy.check(Capability::PackAccess, None),
        Decision::Deny(DenyReason::NotGranted)
    );
    for tier in BuiltinRole::ALL {
        assert!(
            !policy.holds_builtin(*tier),
            "{tier:?} must not be conferred"
        );
    }
    assert_eq!(
        policy.check(Capability::CredentialsManage, None),
        Decision::Deny(DenyReason::Reserved),
        "an admin-reserved capability is not reachable by rank alone"
    );

    // Assigning that same role is subject to the identical containment rule, so a
    // delegator that cannot read `config/**` may not hand the role out at all.
    let narrow = delegator(
        &fixture,
        &owner,
        "Subtree delegator",
        30,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::FilesRead, "worlds/**"),
        ],
    );
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    assert_denied(
        remote(
            &fixture,
            &narrow,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: narrow_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotDelegable,
        "assigning the config reader role out of its own scope",
    );
    assert_eq!(
        snapshot_of(&fixture, &viewer).rank(),
        BuiltinRole::Viewer.rank()
    );
    // An actor that does hold `config/**` may assign it, and the effect is exactly
    // the role's own scope.
    expect_assignment(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: narrow_role,
                expiry: Expiry::Never,
            },
        ),
        "the administrator assigns it",
    );
    assert_eq!(
        snapshot_of(&fixture, &viewer)
            .check(Capability::FilesRead, Some(&resource("config/options.txt"))),
        Decision::Allow
    );
    assert_eq!(
        snapshot_of(&fixture, &viewer).rank(),
        15,
        "and the custom role replaced the viewer's own tier"
    );
}

// ================================================= C: overrides through delegation

#[test]
fn an_override_allow_must_be_contained_and_a_deny_still_needs_roles_assign() {
    let fixture = Fixture::open();
    let owner = fixture.owner();

    let narrow = delegator(
        &fixture,
        &owner,
        "Override narrow",
        30,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::FilesRead, "worlds/**"),
        ],
    );
    let reader_only = delegator(
        &fixture,
        &owner,
        "Override reader",
        30,
        vec![grant(Capability::FilesRead, "*")],
    );
    let conflicted = delegator(
        &fixture,
        &owner,
        "Override conflicted",
        31,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::FilesRead, "*"),
        ],
    );
    fixture.stored_override(
        &conflicted,
        Capability::FilesRead,
        "worlds/**",
        Effect::Deny,
        None,
    );

    let wide_target = fixture.subject(builtin(BuiltinRole::PackUser));
    let deny_target = fixture.subject(builtin(BuiltinRole::PackUser));
    let refused_target = fixture.subject(builtin(BuiltinRole::PackUser));
    let conflicted_target = fixture.subject(builtin(BuiltinRole::PackUser));
    let peer = delegator(
        &fixture,
        &owner,
        "Override peer",
        31,
        vec![grant(Capability::RolesAssign, "*")],
    );

    // An allow must lie inside the actor's own scope.
    assert_denied(
        remote(
            &fixture,
            &narrow,
            RemoteCommand::SetOverride {
                principal: wide_target.principal.clone(),
                grant: grant(Capability::FilesRead, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotDelegable,
        "a whole-scope allow",
    );
    expect_override(
        remote(
            &fixture,
            &narrow,
            RemoteCommand::SetOverride {
                principal: wide_target.principal.clone(),
                grant: grant(Capability::FilesRead, "worlds/maps"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        "a contained allow",
    );
    let wide = snapshot_of(&fixture, &wide_target);
    assert_eq!(
        wide.check(Capability::FilesRead, Some(&resource("worlds/maps"))),
        Decision::Allow,
        "the contained allow is effective at its own exact path"
    );
    assert_eq!(
        wide.check(
            Capability::FilesRead,
            Some(&resource("worlds/maps/atlas.png"))
        ),
        Decision::Deny(DenyReason::NotGranted),
        "an exact allow grants one path, never a subtree"
    );
    assert_eq!(
        wide.check(Capability::FilesRead, Some(&resource("config/options.txt"))),
        Decision::Deny(DenyReason::NotGranted),
        "and never anything outside its own scope"
    );

    // A deny cannot widen authority, so it is accepted for a scope the delegator
    // holds nothing at all: `authorization::override_grant` returns Allow for
    // `Effect::Deny` without consulting containment at all.
    expect_override(
        remote(
            &fixture,
            &narrow,
            RemoteCommand::SetOverride {
                principal: deny_target.principal.clone(),
                grant: grant(Capability::FilesRead, "*"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
        "a deny the delegator cannot itself hold",
    );
    for text in ["config/options.txt", "worlds/maps/atlas.png"] {
        assert_eq!(
            snapshot_of(&fixture, &deny_target).check(Capability::FilesRead, Some(&resource(text))),
            Decision::Deny(DenyReason::ExplicitDeny),
            "{text}: a delegator may deny more than it holds"
        );
    }

    // ACTUAL BEHAVIOUR, which is stricter than "a deny needs nothing at all": the
    // exact `roles.assign` check runs before the effect is examined, so a delegator
    // that holds no delegation capability cannot set even a narrowing deny.
    assert_denied(
        remote(
            &fixture,
            &reader_only,
            RemoteCommand::SetOverride {
                principal: refused_target.principal.clone(),
                grant: grant(Capability::FilesRead, "*"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotGranted,
        "a deny without roles.assign",
    );
    assert_eq!(
        scalar::<i64>(
            &fixture,
            &format!(
                "SELECT count(*) FROM principal_overrides WHERE principal_id = '{}'",
                refused_target.principal.as_str()
            )
        ),
        0,
        "the refused deny left no row"
    );

    // An overlapping deny removes the actor's own authority over that scope, so the
    // same actor may no longer allow it â€¦
    assert_denied(
        remote(
            &fixture,
            &conflicted,
            RemoteCommand::SetOverride {
                principal: conflicted_target.principal.clone(),
                grant: grant(Capability::FilesRead, "worlds/maps"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotDelegable,
        "an allow over an overlapping deny",
    );
    // â€¦ but may still deny it, and may still allow a scope no deny touches.
    expect_override(
        remote(
            &fixture,
            &conflicted,
            RemoteCommand::SetOverride {
                principal: conflicted_target.principal.clone(),
                grant: grant(Capability::FilesRead, "worlds/maps"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
        "a deny over an overlapping deny",
    );
    expect_override(
        remote(
            &fixture,
            &conflicted,
            RemoteCommand::SetOverride {
                principal: conflicted_target.principal.clone(),
                grant: grant(Capability::FilesRead, "config/a.txt"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        "an allow clear of every deny",
    );

    // Local-only capabilities are refused before the target is even examined, for
    // both effects, so not even a deny of local power can be written.
    for effect in [Effect::Allow, Effect::Deny] {
        assert_denied(
            remote(
                &fixture,
                &owner,
                RemoteCommand::SetOverride {
                    principal: wide_target.principal.clone(),
                    grant: grant(Capability::OwnerRecovery, "*"),
                    effect,
                    expiry: Expiry::Never,
                },
            ),
            DenyReason::LocalOnly,
            "a local-only override",
        );
    }
    // Owner-reserved powers are refused for an allow even though the Owner holds
    // them, so no per-principal row can widen a role later.
    assert_denied(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: wide_target.principal.clone(),
                grant: grant(Capability::BackupsRestore, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::OwnerReserved,
        "an owner-reserved allow",
    );

    // The same strictly-lower, no-self hierarchy applies to overrides.
    assert_denied(
        remote(
            &fixture,
            &conflicted,
            RemoteCommand::SetOverride {
                principal: conflicted.principal.clone(),
                grant: grant(Capability::StatusView, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::SelfTarget,
        "a self target",
    );
    assert_denied(
        remote(
            &fixture,
            &conflicted,
            RemoteCommand::SetOverride {
                principal: peer.principal.clone(),
                grant: grant(Capability::StatusView, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::Rank,
        "an equal-rank target",
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn revocation_requires_roles_revoke_and_the_same_strict_hierarchy() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    let pack_user = fixture.subject(builtin(BuiltinRole::PackUser));
    // `roles.assign` but not `roles.revoke`: the two capabilities are independent.
    let assigner = delegator(
        &fixture,
        &owner,
        "Assigner",
        30,
        vec![grant(Capability::RolesAssign, "*")],
    );

    let pack_user_assignment = expect_assignment(
        remote(
            &fixture,
            &owner,
            RemoteCommand::AssignRole {
                principal: pack_user.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        "owner assigns viewer",
    );
    let owner_own_assignment = live_assignment(&fixture, &owner.principal);
    let administrator_own_assignment = live_assignment(&fixture, &administrator.principal);
    let administrator_own_override = OverrideId::parse(&fixture.stored_override(
        &administrator,
        Capability::FilesRead,
        "logs/**",
        Effect::Allow,
        None,
    ))
    .expect("stored override id");
    let owner_own_override = OverrideId::parse(&fixture.stored_override(
        &owner,
        Capability::FilesRead,
        "logs/**",
        Effect::Allow,
        None,
    ))
    .expect("stored override id");
    let owner_held_override = expect_override(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: administrator.principal.clone(),
                grant: grant(Capability::FilesRead, "logs/**"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        "owner overrides the administrator",
    );

    // `roles.revoke` is exact.
    assert_denied(
        remote(
            &fixture,
            &assigner,
            RemoteCommand::RevokeAssignment(pack_user_assignment.clone()),
        ),
        DenyReason::NotGranted,
        "revoke assignment without roles.revoke",
    );
    assert_denied(
        remote(
            &fixture,
            &assigner,
            RemoteCommand::RevokeOverride(administrator_own_override.clone()),
        ),
        DenyReason::NotGranted,
        "revoke override without roles.revoke",
    );
    assert_eq!(active_role_ids(&fixture, &pack_user.principal).len(), 1);

    // A grant belonging to a higher-ranked principal, and one's own grant, are both
    // refused: `rank` for the former and `self` for the latter, because
    // `lower_target` compares identity before rank.
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeOverride(owner_own_override),
        ),
        DenyReason::Rank,
        "revoking a grant held by a higher principal",
    );
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeAssignment(live_assignment(&fixture, &owner.principal)),
        ),
        DenyReason::Rank,
        "revoking the owner's own assignment from below",
    );
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeAssignment(administrator_own_assignment.clone()),
        ),
        DenyReason::SelfTarget,
        "revoking one's own assignment",
    );
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeOverride(administrator_own_override),
        ),
        DenyReason::SelfTarget,
        "revoking one's own override",
    );
    assert_denied(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeAssignment(owner_own_assignment),
        ),
        DenyReason::SelfTarget,
        "the owner cannot revoke its own owner assignment",
    );
    assert!(
        fixture.identities.owner_exists().expect("owner exists"),
        "bootstrap must stay closed after the refused self-revocation"
    );
    assert_eq!(
        active_role_ids(&fixture, &owner.principal),
        vec![builtin(BuiltinRole::Owner).as_str().to_owned()],
        "the owner's own row is untouched"
    );
    assert_eq!(
        active_role_ids(&fixture, &administrator.principal),
        vec![builtin(BuiltinRole::Administrator).as_str().to_owned()],
        "the refused revocations changed nothing"
    );

    // A strictly lower grant is revocable, and revocation is not idempotent.
    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeAssignment(pack_user_assignment.clone()),
        ),
        "a remote revocation",
    );
    assert!(active_role_ids(&fixture, &pack_user.principal).is_empty());
    assert_no_authority_at_all(&fixture, &pack_user, "revoked assignment");
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeAssignment(pack_user_assignment),
        ),
        IdentityErrorCode::NotFound,
        "an already revoked assignment",
    );
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeAssignment(
                AssignmentId::parse("no-such-assignment").expect("assignment id"),
            ),
        ),
        IdentityErrorCode::NotFound,
        "an unknown assignment",
    );

    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeOverride(owner_held_override.clone()),
        ),
        "a remote revocation",
    );
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeOverride(owner_held_override),
        ),
        IdentityErrorCode::NotFound,
        "an already revoked override",
    );
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeOverride(
                OverrideId::parse("no-such-override").expect("override id"),
            ),
        ),
        IdentityErrorCode::NotFound,
        "an unknown override",
    );
    assert_last_rejection(
        &fixture,
        "authority.override.revoke",
        IdentityErrorCode::NotFound,
    );

    // The hierarchy works in both directions: the Owner may revoke an
    // administrator's own assignment, and only then does that principal lose
    // authority rather than only its role.
    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeAssignment(administrator_own_assignment.clone()),
        ),
        "a remote revocation",
    );
    assert_no_authority_at_all(&fixture, &administrator, "revoked administrator");
    assert_success_audit(&fixture, "authority.role.revoke");
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ================================================ D: device revocation hierarchy

/// Java: `deviceRevocationHonorsOwnerDenyAndStrictHierarchyWithoutSelfBypass`
/// (the deny half).
#[test]
fn an_explicit_deny_on_devices_revoke_beats_even_the_owner() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    fixture.stored_override(&owner, Capability::DevicesRevoke, "*", Effect::Deny, None);
    fixture.stored_override(
        &administrator,
        Capability::DevicesRevoke,
        "*",
        Effect::Deny,
        None,
    );
    let before = authority_counts(&fixture);

    // Deny precedence: the exact capability check runs before the Owner membership
    // branch, so the deny applies to every role, the Owner included.
    assert_denied(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        DenyReason::ExplicitDeny,
        "owner under an explicit deny",
    );
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        DenyReason::ExplicitDeny,
        "administrator under an explicit deny",
    );
    assert_eq!(authority_counts(&fixture), before, "no device was revoked");
    assert!(snapshot_of(&fixture, &viewer).has_current_identity());
    assert!(snapshot_of(&fixture, &owner).has_current_identity());
    assert_last_refusal(&fixture, "identity.device.revoke", DenyReason::ExplicitDeny);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

/// Java: the Owner revokes any device. The assertion that it may revoke its *own*
/// device documents the real behaviour rather than a hoped-for one.
#[test]
fn an_owner_may_revoke_any_device_including_its_own() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let spare = plant_device(&fixture, &viewer, "Viewer spare");

    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        "a remote revocation",
    );
    // A revoked device resolves no authority at all.
    assert_resolves_no_authority(&fixture, Some(&viewer), "revoked device");
    // â€¦ and the principal's remaining authority is untouched: the primary role
    // survives and a second device of the same principal still resolves.
    assert_eq!(active_role_ids(&fixture, &viewer.principal).len(), 1);
    let surviving = Subject {
        principal: viewer.principal.clone(),
        device: spare.clone(),
    };
    let policy = snapshot_of(&fixture, &surviving);
    assert!(policy.has_current_identity());
    assert_eq!(policy.rank(), BuiltinRole::Viewer.rank());
    assert_eq!(policy.device(), Some(&spare));

    // Java: the device row is still listed, and reports itself as revoked.
    let record = fixture
        .identities
        .subject_record(&viewer.principal)
        .expect("subject record")
        .expect("record exists");
    assert_eq!(record.devices.len(), 2, "revocation is not a deletion");
    let revoked = record
        .devices
        .iter()
        .find(|device| device.id == viewer.device)
        .expect("the revoked device is still listed");
    assert!(revoked.revoked());
    assert!(revoked.revoked_at.is_some());
    let live = record
        .devices
        .iter()
        .find(|device| device.id == spare)
        .expect("the spare is listed");
    assert!(!live.revoked());

    // Revoking a revoked device, or an unknown one, is `not_found`. The device is
    // resolved before the capability is checked, so this precedes every decision.
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        IdentityErrorCode::NotFound,
        "an already revoked device",
    );
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(DeviceId::parse("no-such-device").expect("device id")),
        ),
        IdentityErrorCode::NotFound,
        "an unknown device",
    );

    // ACTUAL BEHAVIOUR: `authorization::revoke_device` returns Allow for the Owner
    // before it ever looks at the target, so the Owner revokes its own device too.
    // That branch is built-in membership, not numeric rank: a reserved tier is never
    // conferred by rank alone, and the capability check still runs first.
    assert!(snapshot_of(&fixture, &owner).holds_builtin(BuiltinRole::Owner));
    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(owner.device.clone()),
        ),
        "a remote revocation",
    );
    assert_resolves_no_authority(&fixture, Some(&owner), "self-revoked owner");
    assert!(
        fixture.identities.owner_exists().expect("owner exists"),
        "the Owner assignment is still active, so bootstrap stays closed"
    );
    assert_denied(
        remote(&fixture, &owner, RemoteCommand::RevokeDevice(spare)),
        DenyReason::NoIdentity,
        "a revoked device cannot act again",
    );
}

/// Java: `deviceRevocationHonorsOwnerDenyAndStrictHierarchyWithoutSelfBypass`
/// (the hierarchy half).
#[test]
fn a_non_owner_revoker_needs_a_strictly_lower_principal_and_never_itself() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    let equal = fixture.subject(builtin(BuiltinRole::Administrator));
    let maintainer = fixture.subject(builtin(BuiltinRole::Maintainer));
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let moderator = fixture.subject(builtin(BuiltinRole::Moderator));

    // Java: an equal-ranked principal is refused, and there is no self bypass.
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeDevice(equal.device.clone()),
        ),
        DenyReason::Rank,
        "revoking an equal-ranked principal's device",
    );
    assert_denied(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeDevice(administrator.device.clone()),
        ),
        DenyReason::SelfTarget,
        "revoking one's own device",
    );
    assert!(snapshot_of(&fixture, &equal).has_current_identity());
    assert!(snapshot_of(&fixture, &administrator).has_current_identity());

    // A strictly lower principal is revocable.
    expect_revoked(
        remote(
            &fixture,
            &administrator,
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        "a remote revocation",
    );
    assert_resolves_no_authority(&fixture, Some(&viewer), "revoked viewer device");
    assert_eq!(
        active_role_ids(&fixture, &viewer.principal).len(),
        1,
        "the role survives the revoked device"
    );

    // A Maintainer has no `devices.revoke` at all.
    assert_denied(
        remote(
            &fixture,
            &maintainer,
            RemoteCommand::RevokeDevice(moderator.device.clone()),
        ),
        DenyReason::NotGranted,
        "maintainer without devices.revoke",
    );
    // The Owner may lend the exact capability, and only then does the hierarchy apply.
    expect_override(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: maintainer.principal.clone(),
                grant: grant(Capability::DevicesRevoke, "*"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        "owner lends devices.revoke",
    );
    expect_revoked(
        remote(
            &fixture,
            &maintainer,
            RemoteCommand::RevokeDevice(moderator.device.clone()),
        ),
        "a remote revocation",
    );
    assert_resolves_no_authority(&fixture, Some(&moderator), "revoked moderator device");
    assert_denied(
        remote(
            &fixture,
            &maintainer,
            RemoteCommand::RevokeDevice(administrator.device.clone()),
        ),
        DenyReason::Rank,
        "60 is below 80",
    );
    // An explicit deny of the delegated capability takes effect for the next command.
    expect_override(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: maintainer.principal.clone(),
                grant: grant(Capability::DevicesRevoke, "*"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
        "owner denies devices.revoke",
    );
    let spare = plant_device(&fixture, &administrator, "Administrator spare");
    assert_denied(
        remote(&fixture, &maintainer, RemoteCommand::RevokeDevice(spare)),
        DenyReason::ExplicitDeny,
        "a delegated capability under an explicit deny",
    );
    assert!(snapshot_of(&fixture, &administrator).has_current_identity());
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_device_bound_to_another_server_resolves_nothing_and_cannot_be_revoked() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let stranger = fixture.subject(builtin(BuiltinRole::Maintainer));
    assert!(snapshot_of(&fixture, &stranger).has_current_identity());

    plant_foreign_server_device(&fixture, &stranger);
    assert_resolves_no_authority(&fixture, Some(&stranger), "a foreign device");
    // `device_principal` filters on the device's and the principal's server id, so an
    // imported row is indistinguishable from an unknown one.
    assert_rejected(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(stranger.device.clone()),
        ),
        IdentityErrorCode::NotFound,
        "a foreign device",
    );
    // The principal itself is untouched and its assignment is still live.
    assert_eq!(active_role_ids(&fixture, &stranger.principal).len(), 1);
    // `subject_record` is server-scoped too, so the imported row is invisible in the
    // listing as well: it is neither listed nor revocable, and nothing else moved.
    let record = fixture
        .identities
        .subject_record(&stranger.principal)
        .expect("subject record")
        .expect("record exists");
    assert!(
        record.devices.is_empty(),
        "a device on another server id is not listed at all"
    );
    assert!(fixture.identities.owner_exists().expect("owner exists"));
    assert_last_rejection(
        &fixture,
        "identity.device.revoke",
        IdentityErrorCode::NotFound,
    );
}

// ============================================ E: authority is reloaded per command

#[test]
fn the_delegators_authority_is_reloaded_inside_the_next_mutation_transaction() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let reader_role = custom_role(
        &fixture,
        &owner,
        "Low reader",
        20,
        vec![grant(Capability::StatusView, "*")],
    );
    let lender = delegator(
        &fixture,
        &owner,
        "Lending delegator",
        30,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::StatusView, "*"),
        ],
    );
    let borrower = delegator(
        &fixture,
        &owner,
        "Borrowed delegator",
        30,
        vec![
            grant(Capability::RolesAssign, "*"),
            grant(Capability::StatusView, "*"),
        ],
    );
    let first = fixture.subject(builtin(BuiltinRole::PackUser));
    let second = fixture.subject(builtin(BuiltinRole::PackUser));

    // Authority present now.
    expect_assignment(
        remote(
            &fixture,
            &lender,
            RemoteCommand::AssignRole {
                principal: first.principal.clone(),
                role: reader_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        "the delegator delegates",
    );
    assert_eq!(snapshot_of(&fixture, &first).rank(), 20);

    // Remove the delegator's own capability. No cache is cleared, no second service
    // instance is involved and no invalidation call exists: `apply_remote` reloads
    // the policy inside its own write transaction, so the very next command already
    // sees the change.
    let lending_assignment = live_assignment(&fixture, &lender.principal);
    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeAssignment(lending_assignment),
        ),
        "the owner revokes the delegator's role",
    );
    let policy = snapshot_of(&fixture, &lender);
    assert!(
        policy.has_current_identity(),
        "identity is independent of authority"
    );
    assert_eq!(policy.rank(), 0, "the role is gone");
    assert!(policy.roles().is_empty(), "no role at all now");
    assert_eq!(
        policy.check(Capability::RolesAssign, None),
        Decision::Deny(DenyReason::NotGranted)
    );

    let before = authority_counts(&fixture);
    assert_denied(
        remote(
            &fixture,
            &lender,
            RemoteCommand::AssignRole {
                principal: second.principal.clone(),
                role: reader_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        DenyReason::NotGranted,
        "the next command sees the revocation",
    );
    assert_eq!(
        authority_counts(&fixture),
        before,
        "a refusal mutates nothing"
    );
    assert_last_refusal(&fixture, "authority.role.assign", DenyReason::NotGranted);

    // The same holds for authority that is narrowed rather than removed: a deny
    // written by the Owner is visible to the borrower's next command with no
    // invalidation step at all.
    expect_assignment(
        remote(
            &fixture,
            &borrower,
            RemoteCommand::AssignRole {
                principal: second.principal.clone(),
                role: reader_role.clone(),
                expiry: Expiry::Never,
            },
        ),
        "the borrower delegates first",
    );
    expect_override(
        remote(
            &fixture,
            &owner,
            RemoteCommand::SetOverride {
                principal: borrower.principal.clone(),
                grant: grant(Capability::RolesAssign, "*"),
                effect: Effect::Deny,
                expiry: Expiry::Never,
            },
        ),
        "owner denies roles.assign to the borrower",
    );
    assert_denied(
        remote(
            &fixture,
            &borrower,
            RemoteCommand::AssignRole {
                principal: first.principal.clone(),
                role: reader_role,
                expiry: Expiry::Never,
            },
        ),
        DenyReason::ExplicitDeny,
        "the next command sees the new deny",
    );
    assert_last_refusal(&fixture, "authority.role.assign", DenyReason::ExplicitDeny);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

/// The compile-level half of E: no command accepts a `PolicySnapshot`.
///
/// Verified statically, not asserted at runtime:
///
/// 1. Every `PolicySnapshot` field is private to `identity/policy.rs`, so no caller outside
///    that module can construct or mutate one. The two constructors are `none()` and the
///    grant-taking `from_resolved`, which is `pub(super)` and whose only production caller is
///    `Repository::policy`, after it has read current authority inside the caller's own
///    transaction.
/// 2. Searching `crates/vesta-control/src` for `PolicySnapshot` yields only: the
///    type and its constructors (`policy.rs`), the private decision functions of
///    `authorization.rs`, the private resolver `Repository::policy`
///    (`repository.rs`), the private closure parameter of `IdentityService::remote`
///    and its private helpers `authenticated_principal` / `remote_actor`
///    (`service.rs`), the `pub use` in `identity/mod.rs`, and the *return* type of
///    `IdentityService::current_policy`. No `RemoteCommand` variant and no parameter
///    of `apply_remote` mentions it, so a snapshot cannot be handed back in.
/// 3. All six `RemoteCommand` variants are therefore constructible from a `Subject`
///    plus plain domain values, which is what the loop below exercises.
///
/// Caveat reported rather than worked around: `from_resolved` is `pub(super)`, which Rust
/// cannot narrow to a single sibling module, so it is reachable from any module under
/// `identity` — including a test module. This is an API-surface guarantee for external
/// callers plus a convention inside the crate, not a language-level barrier.
#[test]
fn a_policy_snapshot_is_observation_only_and_never_accepted_by_a_command() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));

    // `PolicySnapshot::none()` is the only policy a caller can build, and it is worth
    // nothing at all.
    let empty = PolicySnapshot::none();
    assert!(!empty.has_current_identity());
    assert_eq!(empty.principal(), None);
    assert_eq!(empty.device(), None);
    assert_eq!(empty.rank(), 0);
    assert!(empty.roles().is_empty());
    for role in BuiltinRole::ALL {
        assert!(!empty.holds_builtin(*role), "{role:?}");
    }
    for capability in Capability::ALL {
        assert_eq!(
            empty.check(*capability, None),
            Decision::Deny(DenyReason::NoIdentity),
            "{} without a resource",
            capability.id()
        );
        assert_eq!(
            empty.check(*capability, Some(&Resource::ROOT)),
            Decision::Deny(DenyReason::NoIdentity),
            "{} with a resource",
            capability.id()
        );
        assert!(
            !empty.holds_entirely(*capability, &ResourceScope::ALL),
            "holds_entirely {}",
            capability.id()
        );
    }

    // A real observation of the Owner is a truthful, maximally privileged
    // description of its authority.
    let observed = snapshot_of(&fixture, &owner);
    assert!(observed.holds_builtin(BuiltinRole::Owner));
    assert_eq!(observed.principal(), Some(&owner.principal));
    assert_eq!(observed.device(), Some(&owner.device));
    assert_eq!(
        observed.check(Capability::RolesAssign, None),
        Decision::Allow
    );
    assert_eq!(
        observed.check(Capability::DevicesRevoke, None),
        Decision::Allow
    );
    assert!(observed.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));

    // It is still only an observation. The authority it describes is gone the moment
    // the device is revoked, while the value in hand keeps claiming "allow": a
    // snapshot is a description of the past and can never be replayed, because no
    // command accepts one.
    expect_revoked(
        remote(
            &fixture,
            &owner,
            RemoteCommand::RevokeDevice(owner.device.clone()),
        ),
        "a remote revocation",
    );
    assert_eq!(
        observed.check(Capability::RolesAssign, None),
        Decision::Allow,
        "the held observation is a snapshot of the past"
    );
    assert_resolves_no_authority(&fixture, Some(&owner), "owner after self-revocation");

    // Every command from a principal whose authority has lapsed is refused with the
    // same reason, whatever the caller may believe it holds.
    let viewer_override = OverrideId::parse(&fixture.stored_override(
        &viewer,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        None,
    ))
    .expect("stored override id");
    let commands = [
        (
            "authority.role.assign",
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        (
            "authority.override.set",
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: grant(Capability::FilesRead, "config/**"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        (
            "authority.role.revoke",
            RemoteCommand::RevokeAssignment(live_assignment(&fixture, &viewer.principal)),
        ),
        (
            "authority.override.revoke",
            RemoteCommand::RevokeOverride(viewer_override),
        ),
        (
            "identity.device.revoke",
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        (
            "authority.role.create",
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Nothing to inherit").expect("role name"),
                    CustomRank::new(15).expect("custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role value"),
            ),
        ),
    ];
    let before = authority_counts(&fixture);
    for (action, command) in commands {
        assert_denied(
            remote(&fixture, &owner, command),
            DenyReason::NoIdentity,
            action,
        );
        assert_last_refusal(&fixture, action, DenyReason::NoIdentity);
    }
    assert_eq!(
        authority_counts(&fixture),
        before,
        "nothing an expired identity attempted changed"
    );
    assert_eq!(
        fixture.count("roles"),
        8,
        "no custom role was created beyond the safe access defaults"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}
