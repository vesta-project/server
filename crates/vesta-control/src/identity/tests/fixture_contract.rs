//! Validates the shared fixture itself, so a broken support helper cannot be mistaken
//! for passing behavioral coverage in another family module.
use super::support::{Fixture, INITIAL_TIME_MS};
use crate::identity::{
    AuthorityChange, BuiltinRole, Capability, CodeLifetime, Expiry, IdentityService, LocalCommand,
    LocalOutcome, PrincipalId, Purpose, RemoteCommand, ResourceScope, RoleGrant, RoleId, Subject,
};
use std::time::{Duration, UNIX_EPOCH};
use vesta_state::Clock;

fn viewer() -> RoleId {
    RoleId::from(BuiltinRole::Viewer)
}

#[test]
fn fixture_pairs_a_real_owner_through_the_production_paths() {
    let fixture = Fixture::open();
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    let owner = fixture.owner();
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(fixture.clock.millis() == INITIAL_TIME_MS);

    let policy = fixture
        .identities
        .current_policy(Some(&owner))
        .expect("current policy");
    assert!(policy.has_current_identity());
    assert!(policy.holds_builtin(BuiltinRole::Owner));

    let record = fixture
        .identities
        .subject_record(&owner.principal)
        .expect("subject record")
        .expect("owner record exists");
    assert_eq!(record.principal.id, owner.principal);
    assert_eq!(record.devices.len(), 1);
    assert!(!record.devices[0].revoked());

    assert_eq!(fixture.count("principals"), 1);
    assert_eq!(fixture.count("devices"), 1);
    assert_eq!(fixture.count("bootstrap_codes"), 1);
    assert_eq!(
        fixture.audit_actions(),
        vec![
            "identity.bootstrap.issue".to_owned(),
            "identity.owner.paired".to_owned(),
        ]
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn production_local_authority_is_unavailable_and_accepts_no_proof() {
    let fixture = Fixture::open();
    let production = IdentityService::new(fixture.db.clone()).expect("production service");
    for proof in [
        fixture.foreign_proof(),
        fixture.proof(Purpose::OwnerBootstrap),
    ] {
        let error = production
            .apply_local(Some(&proof), LocalCommand::Bootstrap(Default::default()))
            .expect_err("production must refuse every proof");
        assert_eq!(error.code(), "local_authority_required");
        assert!(matches!(error, crate::ControlError::Rejected { .. }));
    }
    let error = production
        .apply_local(None, LocalCommand::Bootstrap(Default::default()))
        .expect_err("production must refuse an absent proof");
    assert_eq!(error.code(), "local_authority_required");
    assert!(!production.owner_exists().expect("owner lookup"));
}

#[test]
fn bootstrap_outcome_exposes_only_the_secret_once() {
    let fixture = Fixture::open();
    let proof = fixture.proof(Purpose::OwnerBootstrap);
    let outcome = fixture
        .identities
        .apply_local(Some(&proof), LocalCommand::Bootstrap(Default::default()))
        .expect("issue bootstrap code");
    let LocalOutcome::Code(code) = outcome else {
        panic!("bootstrap returns a code outcome");
    };
    assert_eq!(code.purpose(), Purpose::OwnerBootstrap);
    assert_eq!(
        code.expires_at_ms(),
        INITIAL_TIME_MS + Duration::from_secs(600).as_millis() as i64
    );
    assert_eq!(
        code.expires_at(),
        UNIX_EPOCH + Duration::from_millis((INITIAL_TIME_MS + 600_000) as u64)
    );
    assert!(!format!("{code:?}").contains(code.expose_secret()));
    // The persisted row keeps only a digest, never the secret itself.
    let stored = fixture
        .db
        .read(|connection| {
            Ok(
                connection.query_row("SELECT code_hash FROM bootstrap_codes", [], |row| {
                    row.get::<_, String>(0)
                })?,
            )
        })
        .expect("read stored code digest");
    assert_ne!(stored, code.expose_secret());
    assert_eq!(stored.len(), 64);
}

#[test]
fn command_lifetimes_are_validated_before_any_transaction_runs() {
    assert!(CodeLifetime::new(Duration::from_secs(600)).is_ok());
    assert!(CodeLifetime::new(Duration::from_secs(59)).is_err());
    assert!(CodeLifetime::new(Duration::from_secs(3_601)).is_err());
    assert!(CodeLifetime::new(Duration::from_secs(600) + Duration::from_micros(1)).is_err());
    assert!(CodeLifetime::new(Duration::ZERO).is_err());
    assert_eq!(
        CodeLifetime::new(Duration::from_secs(600)).expect("valid"),
        CodeLifetime::default()
    );
    assert_ne!(
        CodeLifetime::new(Duration::from_secs(120)).expect("valid"),
        CodeLifetime::default()
    );

    // A relative expiry that cannot be stored at millisecond precision, or that would
    // overflow, is rejected as a command value rather than silently rounded.
    assert!(
        Expiry::After(Duration::ZERO)
            .deadline(INITIAL_TIME_MS)
            .is_err()
    );
    assert!(
        Expiry::After(Duration::from_nanos(1))
            .deadline(INITIAL_TIME_MS)
            .is_err()
    );
    assert!(
        Expiry::After(Duration::from_millis(1_500) + Duration::from_micros(1))
            .deadline(INITIAL_TIME_MS)
            .is_err()
    );
    assert_eq!(
        Expiry::After(Duration::from_millis(1_500))
            .deadline(INITIAL_TIME_MS)
            .expect("exact millisecond expiry"),
        Some(INITIAL_TIME_MS + 1_500)
    );
    assert_eq!(
        Expiry::Never.deadline(INITIAL_TIME_MS).expect("permanent"),
        None
    );
    assert_eq!(
        Expiry::After(Duration::from_secs(30))
            .deadline(INITIAL_TIME_MS)
            .expect("relative expiry"),
        Some(INITIAL_TIME_MS + 30_000)
    );
}

#[test]
fn owner_delegates_a_lower_role_and_the_grant_is_audited_atomically() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let target = fixture.subject(viewer());
    let change = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: target.principal.clone(),
                role: viewer(),
                expiry: Expiry::Never,
            },
        )
        .expect("owner delegates viewer");
    assert!(matches!(change, AuthorityChange::Assignment(_)));
    assert!(
        fixture
            .identities
            .current_policy(Some(&target))
            .expect("target policy")
            .holds_builtin(BuiltinRole::Viewer)
    );
    // Owner, fixture subject and the replacement grant all keep a row: replacement
    // revokes the previous assignment rather than deleting or reusing it.
    assert_eq!(fixture.count("role_assignments"), 3);
    assert_eq!(fixture.count("audit_events"), 4);
    assert_eq!(
        fixture.audit_actions(),
        vec![
            "identity.bootstrap.issue".to_owned(),
            "identity.owner.paired".to_owned(),
            "authority.role.assign".to_owned(),
            "authority.role.assign".to_owned(),
        ]
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_subject_is_only_a_lookup_key_not_authenticated_authority() {
    let fixture = Fixture::open();
    let invented = Subject {
        principal: PrincipalId::parse("p-1").expect("principal"),
        device: crate::identity::DeviceId::parse("d-1").expect("device"),
    };
    let policy = fixture
        .identities
        .current_policy(Some(&invented))
        .expect("current policy");
    assert!(!policy.has_current_identity());
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(fixture.count("principals"), 0);
    assert_eq!(fixture.count("devices"), 0);
}

#[test]
fn scope_and_capability_values_are_validated_at_construction() {
    let scope = ResourceScope::parse("worlds/**").expect("subtree scope");
    let grant = RoleGrant::new(Capability::FilesRead, scope.clone()).expect("scoped grant");
    assert_eq!(grant.capability(), Capability::FilesRead);
    assert_eq!(grant.scope().text(), "worlds/**");
    // A capability that is not resource-scoped may not carry a narrower scope.
    assert!(RoleGrant::new(Capability::StatusView, scope).is_err());
    assert!(RoleGrant::new(Capability::FilesRead, ResourceScope::ALL).is_ok());
    assert_eq!(Capability::ALL.len(), 54);
    assert_eq!(BuiltinRole::ALL.len(), 6);
}

#[test]
fn local_proof_is_opaque_redacted_and_refused_in_production() {
    // `LocalProof` has private fields and no public constructor, parser or
    // deserializer. Its Debug rendering never discloses channel evidence, and the
    // production service refuses a proof that looks perfectly well formed.
    let fixture = Fixture::open();
    let proof = fixture.proof(Purpose::OwnerBootstrap);
    assert_eq!(format!("{proof:?}"), "LocalProof[<redacted>]");
    assert!(!format!("{proof:?}").contains("test-os-user"));
    let production = IdentityService::new(fixture.db.clone()).expect("production service");
    assert_eq!(
        production
            .apply_local(Some(&proof), LocalCommand::Bootstrap(Default::default()))
            .expect_err("production refuses every proof")
            .code(),
        "local_authority_required"
    );
    assert_eq!(Purpose::OwnerBootstrap.id(), "owner_bootstrap");
    assert_eq!(Purpose::OwnerRecovery.id(), "owner_recovery");
    assert_eq!(Purpose::LocalRoleGrant.id(), "local_role_grant");
}
