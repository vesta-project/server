//! Bootstrap and local-recovery evidence: local-proof refusal, single-use secret-bearing
//! codes, audit atomicity under an injected audit fault, concurrent redemption, and the
//! administrative-access revocation performed by a recovery code.
//!
//! Every case runs against real SQLite state, the real audit chain and the test clock.
//! Nothing here mocks the service, the audit log, the database or the local authority.
use super::support::*;
use crate::identity::{
    AuthorityChange, BuiltinRole, Capability, CodeInput, CodeLifetime, Decision, DenyReason,
    DeviceLabel, DisplayName, Fingerprint, IdentityService, LocalCommand, LocalOutcome, LocalProof,
    OwnerAdded, Purpose, RecoveryAction, RedeemOwner, RemoteCommand, RoleId,
};
use crate::{ControlError, Result};
use base64::{
    Engine as _, engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE_NO_PAD,
};
use sha2::{Digest, Sha256};
use std::time::Duration;
use vesta_state::Clock;
use vesta_state::audit::AuditEntry;

const MIN_CODE_LIFETIME: Duration = Duration::from_secs(60);
const DEFAULT_CODE_TTL_MS: i64 = 600_000;

// ---------------------------------------------------------------------------- helpers

fn role(builtin: BuiltinRole) -> RoleId {
    RoleId::from(builtin)
}

/// A canonical 20-byte Base64URL token that this database has never issued.
fn never_issued_code() -> String {
    URL_SAFE_NO_PAD.encode([0x5a_u8; 20])
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// A distinct nonce per case, comfortably inside the accepted 16..=256 byte window.
/// Nonce bounds are UTF-8 BYTE counts, not Java `String.length()` UTF-16 counts.
fn nonce_for(tag: &str) -> String {
    format!("vesta-bootstrap-test-nonce-{tag:0<32}")
}

fn attestation(
    nonce: &str,
    purpose: Purpose,
    os_actor: &str,
    issued_at_ms: i64,
    expires_at_ms: i64,
) -> Attestation {
    Attestation {
        nonce: nonce.to_owned(),
        purpose,
        os_actor: os_actor.to_owned(),
        issued_at_ms,
        expires_at_ms,
    }
}

fn max_proof_lifetime_ms() -> i64 {
    i64::try_from(crate::identity::domain::MAX_PROOF_LIFETIME.as_millis())
        .expect("MAX_PROOF_LIFETIME fits in i64 milliseconds")
}

fn proof_clock_skew_ms() -> i64 {
    i64::try_from(crate::identity::domain::PROOF_CLOCK_SKEW.as_millis())
        .expect("PROOF_CLOCK_SKEW fits in i64 milliseconds")
}

fn audit_entries(fixture: &Fixture) -> Vec<AuditEntry> {
    fixture.audit.page(0, 500).expect("read audit page")
}

fn rejected_entries(fixture: &Fixture) -> Vec<AuditEntry> {
    audit_entries(fixture)
        .into_iter()
        .filter(|entry| entry.outcome == "rejected")
        .collect()
}

/// Successful audit rows for one action. A refusal is audited under the same action
/// name with `outcome = "rejected"`, so success rows are counted separately.
fn count_success(fixture: &Fixture, action: &str) -> usize {
    audit_entries(fixture)
        .iter()
        .filter(|entry| entry.action == action && entry.outcome == "success")
        .count()
}

/// Everything an operator could read back out of the audit chain for this server.
fn audit_text(fixture: &Fixture) -> String {
    audit_entries(fixture)
        .iter()
        .map(|entry| format!("{entry:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `(code_hash, consumed_at, invalidated_at)` in insertion order.
fn code_rows(fixture: &Fixture) -> Vec<(String, Option<i64>, Option<i64>)> {
    fixture
        .db
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT code_hash, consumed_at, invalidated_at FROM bootstrap_codes ORDER BY rowid",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read bootstrap code rows")
}

fn nonce_digests(fixture: &Fixture) -> Vec<String> {
    fixture
        .db
        .read(|connection| {
            let mut statement =
                connection.prepare("SELECT nonce_hash FROM local_authority_uses ORDER BY rowid")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read nonce digests")
}

fn redeem_key(
    identities: &IdentityService,
    code: &str,
    name: &str,
    device: &str,
    key: Fingerprint,
) -> Result<OwnerAdded> {
    identities.redeem_owner(RedeemOwner {
        code: CodeInput::parse(code)?,
        name: DisplayName::parse(name)?,
        device_label: DeviceLabel::parse(device)?,
        fingerprint: key,
    })
}

fn subject_of(added: &OwnerAdded) -> Subject {
    Subject {
        principal: added.principal.clone(),
        device: added.device.clone(),
    }
}

fn issue_code(
    fixture: &Fixture,
    proof: &LocalProof,
    command: LocalCommand,
) -> Result<(String, i64)> {
    match fixture.identities.apply_local(Some(proof), command)? {
        LocalOutcome::Code(code) => {
            // The secret is read exactly once, here, and never through Debug.
            let secret = code.expose_secret().to_owned();
            assert!(
                !format!("{code:?}").contains(&secret),
                "BootstrapCode Debug must not disclose the secret"
            );
            Ok((secret, code.expires_at_ms()))
        }
        other => panic!("a code-issuing command must return a code, got {other:?}"),
    }
}

fn bootstrap_code(fixture: &Fixture) -> (String, i64) {
    bootstrap_code_with(fixture, CodeLifetime::default())
}

fn bootstrap_code_with(fixture: &Fixture, lifetime: CodeLifetime) -> (String, i64) {
    let proof = fixture.proof(Purpose::OwnerBootstrap);
    issue_code(fixture, &proof, LocalCommand::Bootstrap(lifetime)).expect("issue bootstrap code")
}

fn recovery_command(action: RecoveryAction) -> LocalCommand {
    LocalCommand::Recover {
        lifetime: CodeLifetime::default(),
        action,
    }
}

fn recovery_code(fixture: &Fixture, action: RecoveryAction) -> (String, i64) {
    let proof = fixture.proof(Purpose::OwnerRecovery);
    issue_code(fixture, &proof, recovery_command(action)).expect("issue recovery code")
}

fn expects_code(error: ControlError, code: &'static str, context: &str) {
    assert_eq!(error.code(), code, "{context}");
    assert!(
        matches!(error, ControlError::Rejected { .. }),
        "{context}: a clean domain refusal, never an infrastructure error, got {error:?}"
    );
}

fn expects_local_authority_refusal(result: Result<LocalOutcome>, context: &str) {
    let error = result.expect_err("the local command must be refused");
    expects_code(error, "local_authority_required", context);
}

/// A refusal-audit failure is an error, never a successful denial, and it retains the
/// original decision.
fn expects_audit_unavailable(error: ControlError, primary: &'static str) {
    assert_eq!(error.code(), "audit_unavailable");
    assert_eq!(error.reason(), None);
    let ControlError::AuditUnavailable { primary: kept, .. } = &error else {
        panic!("expected ControlError::AuditUnavailable, got {error:?}");
    };
    assert_eq!(
        kept.code(),
        primary,
        "the original refusal must be preserved"
    );
}

/// Infrastructure failures stay errors and are never reported as a decision.
fn expects_infrastructure_failure(error: ControlError, context: &str) {
    assert!(
        matches!(error, ControlError::State(_)),
        "{context}: expected an infrastructure error, got {error:?}"
    );
    assert!(
        error.code().starts_with("state_"),
        "{context}: state errors carry a state_* code, got {}",
        error.code()
    );
    for decision in [
        "local_authority_required",
        "bootstrap_closed",
        "code_invalid",
        "conflict",
    ] {
        assert_ne!(error.code(), decision, "{context}: never a decision code");
    }
}

fn device_fingerprint(fixture: &Fixture, subject: &Subject) -> String {
    fixture
        .identities
        .subject_record(&subject.principal)
        .expect("subject record")
        .expect("recorded subject")
        .devices
        .into_iter()
        .find(|device| device.id == subject.device)
        .expect("the subject device")
        .fingerprint
        .as_str()
        .to_owned()
}

fn device_revoked(fixture: &Fixture, subject: &Subject) -> bool {
    fixture
        .identities
        .subject_record(&subject.principal)
        .expect("subject record")
        .expect("recorded subject")
        .devices
        .into_iter()
        .find(|device| device.id == subject.device)
        .expect("the subject device")
        .revoked()
}

// ------------------------ Java: absentOrForeignLocalProofNeverBootstrapsAndRefusalIsAudited

#[test]
fn absent_and_foreign_local_proof_never_bootstrap_and_each_refusal_is_audited() {
    let fixture = Fixture::open();
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));

    let absent = fixture
        .identities
        .apply_local(None, LocalCommand::Bootstrap(CodeLifetime::default()))
        .expect_err("an absent proof cannot bootstrap");
    expects_code(absent, "local_authority_required", "absent proof");

    let foreign = fixture.foreign_proof();
    assert_eq!(format!("{foreign:?}"), "LocalProof[<redacted>]");
    let foreign = fixture
        .identities
        .apply_local(
            Some(&foreign),
            LocalCommand::Bootstrap(CodeLifetime::default()),
        )
        .expect_err("a foreign proof cannot bootstrap");
    expects_code(foreign, "local_authority_required", "foreign proof");

    assert_eq!(fixture.count("bootstrap_codes"), 0);
    assert_eq!(fixture.count("local_authority_uses"), 0);
    assert_eq!(fixture.count("principals"), 0);
    assert_eq!(fixture.count("devices"), 0);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));

    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 2);
    for refusal in &refusals {
        assert_eq!(refusal.action, "identity.bootstrap.issue");
        assert_eq!(refusal.source, "local-authority");
        assert_eq!(refusal.target, "owner");
        // The only refusal metadata key is `reason`.
        assert_eq!(
            refusal.metadata_json,
            "{\"reason\":\"local_authority_required\"}"
        );
    }
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_well_shaped_local_proof_is_still_refused_by_the_production_authority() {
    // `IdentityService::new` always selects the unavailable authority, so a proof that is
    // perfect in every field cannot reach a real deployment.
    let fixture = Fixture::open();
    let production = IdentityService::new(fixture.db.clone()).expect("production service");
    let proof = fixture.proof(Purpose::OwnerBootstrap);
    let commands = [
        LocalCommand::Bootstrap(CodeLifetime::default()),
        recovery_command(RecoveryAction::KeepAccess),
        LocalCommand::AssignRole {
            principal: parse_principal("production-authority-check"),
            role: role(BuiltinRole::Administrator),
            expiry: crate::identity::Expiry::Never,
        },
    ];
    for command in commands {
        let error = production
            .apply_local(Some(&proof), command)
            .expect_err("production local authority is unavailable");
        expects_code(error, "local_authority_required", "production authority");
    }
    assert_eq!(fixture.count("bootstrap_codes"), 0);
    assert_eq!(fixture.count("local_authority_uses"), 0);
    assert_eq!(fixture.count("principals"), 0);
    assert_eq!(rejected_entries(&fixture).len(), 3);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ------------- Java: invalidLocalAttestationsCannotConsumeANonceOrGrantOwner

#[test]
fn every_malformed_attestation_is_refused_without_consuming_a_nonce_or_a_code() {
    let fixture = Fixture::open();
    let now = fixture.clock.millis();
    let max_lifetime = max_proof_lifetime_ms();
    let skew = proof_clock_skew_ms();

    let cases: Vec<(&str, Attestation)> = vec![
        (
            "owner-recovery-purpose",
            attestation(
                &nonce_for("owner-recovery-purpose"),
                Purpose::OwnerRecovery,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "local-role-grant-purpose",
            attestation(
                &nonce_for("local-role-grant-purpose"),
                Purpose::LocalRoleGrant,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "nonce-below-minimum",
            attestation(
                "aaaaaaaaaaaaaaa",
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "nonce-above-maximum",
            attestation(
                &"n".repeat(257),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "blank-os-actor",
            attestation(
                &nonce_for("blank-os-actor"),
                Purpose::OwnerBootstrap,
                "",
                now,
                now + 60_000,
            ),
        ),
        (
            "whitespace-only-os-actor",
            attestation(
                &nonce_for("whitespace-only-os-actor"),
                Purpose::OwnerBootstrap,
                " \t\u{00a0}\u{2003}\u{3000}",
                now,
                now + 60_000,
            ),
        ),
        (
            "newline-in-os-actor",
            attestation(
                &nonce_for("newline-in-os-actor"),
                Purpose::OwnerBootstrap,
                "bad\nactor",
                now,
                now + 60_000,
            ),
        ),
        (
            "nul-in-os-actor",
            attestation(
                &nonce_for("nul-in-os-actor"),
                Purpose::OwnerBootstrap,
                "actor\u{0000}name",
                now,
                now + 60_000,
            ),
        ),
        (
            "delete-in-os-actor",
            attestation(
                &nonce_for("delete-in-os-actor"),
                Purpose::OwnerBootstrap,
                "actor\u{007f}",
                now,
                now + 60_000,
            ),
        ),
        (
            "os-actor-above-scalar-bound",
            attestation(
                &nonce_for("os-actor-above-scalar-bound"),
                Purpose::OwnerBootstrap,
                &"o".repeat(129),
                now,
                now + 60_000,
            ),
        ),
        (
            "os-actor-above-byte-bound",
            attestation(
                &nonce_for("os-actor-above-byte-bound"),
                Purpose::OwnerBootstrap,
                // 129 four-byte scalars: 516 UTF-8 bytes, past the 512 byte bound.
                &"\u{1f600}".repeat(129),
                now,
                now + 60_000,
            ),
        ),
        (
            "issued-beyond-clock-skew",
            attestation(
                &nonce_for("issued-beyond-clock-skew"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now + skew + 1,
                now + skew + 60_001,
            ),
        ),
        (
            "expires-at-exact-deadline",
            attestation(
                &nonce_for("expires-at-exact-deadline"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now - 1_000,
                now,
            ),
        ),
        (
            "zero-lifetime",
            attestation(
                &nonce_for("zero-lifetime"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now,
            ),
        ),
        (
            "negative-lifetime",
            attestation(
                &nonce_for("negative-lifetime"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now + 2_000,
                now + 1_000,
            ),
        ),
        (
            // Issued and expiring at the same future instant that the clock-skew rule
            // still tolerates: only the non-positive lifetime rule can refuse this.
            "zero-lifetime-inside-clock-skew",
            attestation(
                &nonce_for("zero-lifetime-inside-clock-skew"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now + 1,
                now + 1,
            ),
        ),
        (
            // Issued exactly at the tolerated skew but expiring before that instant:
            // again only the non-positive lifetime rule can refuse this.
            "negative-lifetime-inside-clock-skew",
            attestation(
                &nonce_for("negative-lifetime-inside-clock-skew"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now + skew,
                now + 1,
            ),
        ),
        (
            "lifetime-above-maximum",
            attestation(
                &nonce_for("lifetime-above-maximum"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + max_lifetime + 1,
            ),
        ),
        (
            "unrepresentable-timestamps",
            attestation(
                &nonce_for("unrepresentable-timestamps"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                i64::MAX,
                i64::MIN,
            ),
        ),
    ];

    let replay_nonce = cases[0].1.nonce.clone();

    for (label, payload) in &cases {
        // The raw evidence object discloses neither the nonce nor its own fields.
        let rendered = format!("{payload:?}");
        assert!(
            rendered.contains("<redacted>") && !rendered.contains(&payload.nonce),
            "{label}: attestation Debug must redact the nonce"
        );
        let proof = fixture.proof_from(payload.clone());
        assert_eq!(
            format!("{proof:?}"),
            "LocalProof[<redacted>]",
            "{label}: the proof object discloses nothing"
        );

        let before = rejected_entries(&fixture).len();
        expects_local_authority_refusal(
            fixture.identities.apply_local(
                Some(&proof),
                LocalCommand::Bootstrap(CodeLifetime::default()),
            ),
            label,
        );
        assert_eq!(
            fixture.count("local_authority_uses"),
            0,
            "{label}: a refused attestation consumes no nonce"
        );
        assert_eq!(fixture.count("bootstrap_codes"), 0, "{label}: no code row");
        assert_eq!(fixture.count("principals"), 0, "{label}: no principal");
        assert!(!fixture.identities.owner_exists().expect("owner lookup"));
        let refusals = rejected_entries(&fixture);
        assert_eq!(refusals.len(), before + 1, "{label}: one refusal audit");
        assert_eq!(
            refusals.last().expect("refusal").action,
            "identity.bootstrap.issue"
        );
        assert_eq!(
            refusals.last().expect("refusal").metadata_json,
            "{\"reason\":\"local_authority_required\"}",
            "{label}"
        );
    }

    // Nothing was consumed: the nonce of the first refused case still works when a
    // well-formed proof presents it.
    let proof = fixture.proof_from(attestation(
        &replay_nonce,
        Purpose::OwnerBootstrap,
        "test-os-user",
        now,
        now + 60_000,
    ));
    let (secret, _) = issue_code(
        &fixture,
        &proof,
        LocalCommand::Bootstrap(CodeLifetime::default()),
    )
    .expect("the nonce of a refused attestation was never consumed");
    assert!(!secret.is_empty());
    assert_eq!(fixture.count("local_authority_uses"), 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_consumed_nonce_cannot_be_replayed_against_a_second_command() {
    let fixture = Fixture::open();
    let now = fixture.clock.millis();
    let nonce = nonce_for("single-use-across-commands");
    let proof = fixture.proof_from(attestation(
        &nonce,
        Purpose::OwnerBootstrap,
        "test-os-user",
        now,
        now + 60_000,
    ));

    let (secret, _) = issue_code(
        &fixture,
        &proof,
        LocalCommand::Bootstrap(CodeLifetime::default()),
    )
    .expect("first bootstrap consumes the nonce");
    assert_eq!(fixture.count("local_authority_uses"), 1);
    assert_eq!(fixture.count("bootstrap_codes"), 1);

    // The same proof value, second use, against different commands.
    for command in [
        LocalCommand::Bootstrap(CodeLifetime::default()),
        recovery_command(RecoveryAction::KeepAccess),
    ] {
        expects_local_authority_refusal(
            fixture.identities.apply_local(Some(&proof), command),
            "replayed nonce",
        );
    }
    assert_eq!(
        fixture.count("local_authority_uses"),
        1,
        "a replay never adds a nonce row"
    );
    assert_eq!(
        fixture.count("bootstrap_codes"),
        1,
        "a replay never issues or invalidates a code"
    );
    assert_eq!(rejected_entries(&fixture).len(), 2);
    assert_eq!(
        code_rows(&fixture)[0].2,
        None,
        "the refused replays did not invalidate the issued code"
    );

    // The refused replays rolled back cleanly: the first code is still redeemable.
    redeem_key(
        &fixture.identities,
        &secret,
        "Owner",
        "Owner device",
        fingerprint("single-use-across-commands"),
    )
    .expect("the first code survived the refused replays");
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(fixture.count("principals"), 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn proof_lifetime_nonce_actor_and_skew_boundaries_are_exact() {
    let fixture = Fixture::open();
    let now = fixture.clock.millis();
    let max_lifetime = max_proof_lifetime_ms();
    let skew = proof_clock_skew_ms();

    // Accepted: exactly MAX_PROOF_LIFETIME, exactly PROOF_CLOCK_SKEW, the 16 byte nonce
    // floor, the 256 byte nonce ceiling, the 128 scalar / 512 byte actor bound, and the
    // smallest possible positive lifetime.
    let accepted: Vec<(&str, Attestation)> = vec![
        (
            "lifetime-exactly-maximum",
            attestation(
                &nonce_for("lifetime-exactly-maximum"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + max_lifetime,
            ),
        ),
        (
            "issued-exactly-at-skew",
            attestation(
                &nonce_for("issued-exactly-at-skew"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now + skew,
                now + skew + 1_000,
            ),
        ),
        (
            "nonce-exactly-minimum",
            attestation(
                "aaaaaaaaaaaaaaaa",
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "nonce-exactly-maximum",
            attestation(
                &"n".repeat(256),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "os-actor-exactly-at-scalar-bound",
            attestation(
                &nonce_for("os-actor-exactly-at-scalar-bound"),
                Purpose::OwnerBootstrap,
                &"o".repeat(128),
                now,
                now + 60_000,
            ),
        ),
        (
            "os-actor-exactly-at-byte-bound",
            attestation(
                &nonce_for("os-actor-exactly-at-byte-bound"),
                Purpose::OwnerBootstrap,
                // 128 four-byte scalars: exactly 512 UTF-8 bytes.
                &"\u{1f600}".repeat(128),
                now,
                now + 60_000,
            ),
        ),
        (
            "one-millisecond-lifetime",
            attestation(
                &nonce_for("one-millisecond-lifetime"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + 1,
            ),
        ),
    ];

    for (label, payload) in &accepted {
        let (_, expires_at_ms) = issue_code(
            &fixture,
            &fixture.proof_from(payload.clone()),
            LocalCommand::Bootstrap(CodeLifetime::default()),
        )
        .unwrap_or_else(|error| panic!("{label} must be accepted, got {error:?}"));
        assert_eq!(expires_at_ms, now + DEFAULT_CODE_TTL_MS, "{label}");
    }

    // Refused: one millisecond past each of the same boundaries.
    let refused: Vec<(&str, Attestation)> = vec![
        (
            "lifetime-one-past-maximum",
            attestation(
                &nonce_for("lifetime-one-past-maximum"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + max_lifetime + 1,
            ),
        ),
        (
            "issued-one-past-skew",
            attestation(
                &nonce_for("issued-one-past-skew"),
                Purpose::OwnerBootstrap,
                "test-os-user",
                now + skew + 1,
                now + skew + 1_001,
            ),
        ),
        (
            "nonce-one-below-minimum",
            attestation(
                "aaaaaaaaaaaaaaa",
                Purpose::OwnerBootstrap,
                "test-os-user",
                now,
                now + 60_000,
            ),
        ),
        (
            "os-actor-one-past-bound",
            attestation(
                &nonce_for("os-actor-one-past-bound"),
                Purpose::OwnerBootstrap,
                &"o".repeat(129),
                now,
                now + 60_000,
            ),
        ),
    ];
    for (label, payload) in &refused {
        expects_local_authority_refusal(
            fixture.identities.apply_local(
                Some(&fixture.proof_from(payload.clone())),
                LocalCommand::Bootstrap(CodeLifetime::default()),
            ),
            label,
        );
    }

    assert_eq!(
        fixture.count("local_authority_uses"),
        accepted.len() as i64,
        "only the accepted proofs consumed a nonce"
    );
    assert_eq!(
        fixture.count("bootstrap_codes"),
        accepted.len() as i64,
        "only the accepted proofs issued a code"
    );
    assert_eq!(rejected_entries(&fixture).len(), refused.len());
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn an_os_actor_is_never_trimmed_and_never_case_folded() {
    let fixture = Fixture::open();
    let now = fixture.clock.millis();
    let proof = fixture.proof_from(attestation(
        &nonce_for("untrimmed-os-actor"),
        Purpose::OwnerBootstrap,
        "  Mixed Case OS-Actor  ",
        now,
        now + 60_000,
    ));
    issue_code(
        &fixture,
        &proof,
        LocalCommand::Bootstrap(CodeLifetime::default()),
    )
    .expect("surrounding spaces are legal evidence; nothing is trimmed");

    let bootstrap = audit_entries(&fixture)
        .into_iter()
        .find(|entry| entry.action == "identity.bootstrap.issue")
        .expect("bootstrap audit");
    assert!(
        bootstrap
            .metadata_json
            .contains("\"os_actor\":\"  Mixed Case OS-Actor  \""),
        "the OS actor is recorded verbatim, got {}",
        bootstrap.metadata_json
    );
    assert_eq!(bootstrap.outcome, "success");
}

// -- Java: auditFailureRollsBackBootstrapNonceCodeAndRedemptionAndCanBeRetried (issue half)

#[test]
fn an_audit_append_failure_rolls_back_the_nonce_and_the_code_and_the_command_is_retryable() {
    let fixture = Fixture::open();
    let proof = fixture.proof(Purpose::OwnerBootstrap);
    assert_eq!(format!("{proof:?}"), "LocalProof[<redacted>]");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::Bootstrap(CodeLifetime::default()),
        )
        .expect_err("a failed success audit rolls the command back");
    expects_infrastructure_failure(error, "bootstrap audit failure");
    assert_eq!(fixture.count("bootstrap_codes"), 0, "no code row survives");
    assert_eq!(
        fixture.count("local_authority_uses"),
        0,
        "no nonce row survives"
    );
    assert_eq!(fixture.count("audit_events"), 0, "no audit row survives");
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));

    // The identical command value, with the identical unconsumed proof, now succeeds.
    fixture.audit_failure(false);
    let (secret, _) = issue_code(
        &fixture,
        &proof,
        LocalCommand::Bootstrap(CodeLifetime::default()),
    )
    .expect("the rolled-back bootstrap is retryable");
    assert!(!secret.is_empty());
    assert_eq!(fixture.count("bootstrap_codes"), 1);
    assert_eq!(fixture.count("local_authority_uses"), 1);
    assert_eq!(
        fixture.audit_actions(),
        vec!["identity.bootstrap.issue".to_owned()],
        "exactly one success audit, no refusal"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// -- Java: auditFailureRollsBackBootstrapNonceCodeAndRedemptionAndCanBeRetried (redeem half)

#[test]
fn a_redemption_audit_failure_rolls_back_the_principal_and_device_and_is_retryable() {
    let fixture = Fixture::open();
    let (code, _) = bootstrap_code(&fixture);
    let audit_rows = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = redeem_key(
        &fixture.identities,
        &code,
        "Owner",
        "Owner device",
        fingerprint("redemption-audit-failure"),
    )
    .expect_err("a failed pairing audit rolls the pairing back");
    expects_infrastructure_failure(error, "redemption audit failure");
    assert_eq!(fixture.count("principals"), 0, "no principal survives");
    assert_eq!(fixture.count("devices"), 0, "no device survives");
    assert_eq!(
        fixture.count("role_assignments"),
        0,
        "no assignment survives"
    );
    assert_eq!(
        fixture.count("audit_events"),
        audit_rows,
        "no audit row survives"
    );
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(
        code_rows(&fixture)[0].1,
        None,
        "the unconsumed code row is still open"
    );

    fixture.audit_failure(false);
    let added = redeem_key(
        &fixture.identities,
        &code,
        "Owner",
        "Owner device",
        fingerprint("redemption-audit-failure"),
    )
    .expect("the unconsumed code is still redeemable");
    assert_eq!(fixture.count("principals"), 1);
    assert_eq!(fixture.count("devices"), 1);
    let policy = fixture
        .identities
        .current_policy(Some(&subject_of(&added)))
        .expect("owner policy");
    assert!(policy.holds_builtin(BuiltinRole::Owner));
    assert!(policy.check(Capability::LifecycleStart, None).allowed());
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// -- Java: failedRefusalAuditReportsAuditUnavailableWithoutMutatingAuthority

#[test]
fn a_refused_bootstrap_whose_audit_also_fails_reports_audit_unavailable() {
    let fixture = Fixture::open();
    fixture.audit_failure(true);

    let error = fixture
        .identities
        .apply_local(None, LocalCommand::Bootstrap(CodeLifetime::default()))
        .expect_err("an absent proof is refused");
    expects_audit_unavailable(error, "local_authority_required");
    assert_eq!(
        fixture.count("audit_events"),
        0,
        "the refusal could not be recorded"
    );
    assert_eq!(fixture.count("bootstrap_codes"), 0);
    assert_eq!(fixture.count("local_authority_uses"), 0);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));

    // With the fault removed the same command becomes a normally audited success.
    fixture.audit_failure(false);
    bootstrap_code(&fixture);
    assert_eq!(count_success(&fixture, "identity.bootstrap.issue"), 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_refused_redemption_whose_audit_also_fails_reports_audit_unavailable() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    let audit_rows = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = redeem_key(
        &fixture.identities,
        &never_issued_code(),
        "Owner",
        "Owner device",
        fingerprint("refusal-audit-failure"),
    )
    .expect_err("an unissued code is refused");
    expects_audit_unavailable(error, "code_invalid");
    assert_eq!(fixture.count("audit_events"), audit_rows);
    assert_eq!(fixture.count("principals"), 1, "authority is unchanged");
    assert_eq!(fixture.count("devices"), 1);
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        fixture
            .identities
            .current_policy(Some(&owner))
            .expect("owner policy")
            .check(Capability::BackupsRestore, None)
            .allowed()
    );

    fixture.audit_failure(false);
    let (code, _) = recovery_code(&fixture, RecoveryAction::KeepAccess);
    redeem_key(
        &fixture.identities,
        &code,
        "Recovered owner",
        "New device",
        fingerprint("refusal-audit-failure-retry"),
    )
    .expect("redemption is retryable once the fault is removed");
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ------------- Java: codeAndProofAreSingleUseSecretFreeAndFingerprintsAreCanonical

#[test]
fn bootstrap_is_closed_while_an_active_owner_assignment_exists_and_reopens_after_recovery() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    let codes = fixture.count("bootstrap_codes");
    let nonces = fixture.count("local_authority_uses");

    let proof = fixture.proof(Purpose::OwnerBootstrap);
    let error = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::Bootstrap(CodeLifetime::default()),
        )
        .expect_err("bootstrap is closed while an owner assignment exists");
    expects_code(error, "bootstrap_closed", "closed bootstrap");
    assert_eq!(fixture.count("bootstrap_codes"), codes, "no code is issued");
    assert_eq!(
        fixture.count("local_authority_uses"),
        nonces,
        "the refusal rolls the nonce back"
    );
    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].action, "identity.bootstrap.issue");
    assert_eq!(
        refusals[0].metadata_json,
        "{\"reason\":\"bootstrap_closed\"}"
    );

    // Recovery revokes the administrative assignment, which reopens bootstrap.
    recovery_code(&fixture, RecoveryAction::RevokeAdministrativeAccess);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        !fixture
            .identities
            .current_policy(Some(&owner))
            .expect("owner policy")
            .has_current_identity()
    );
    let (code, _) = bootstrap_code(&fixture);
    let added = redeem_key(
        &fixture.identities,
        &code,
        "Second owner",
        "Second device",
        fingerprint("reopened-bootstrap"),
    )
    .expect("bootstrap reopens once no owner assignment remains");
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(fixture.count("principals"), 2);
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(&added)))
            .expect("new owner policy")
            .holds_builtin(BuiltinRole::Owner)
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn an_outstanding_bootstrap_code_cannot_pair_a_second_owner() {
    let fixture = Fixture::open();
    let (code, _) = bootstrap_code(&fixture);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));

    // An Owner assignment granted through the local role path closes bootstrap just as
    // a paired owner would, so an already-issued code must not mint a second owner.
    let existing = fixture.subject(role(BuiltinRole::Owner));
    assert!(fixture.identities.owner_exists().expect("owner lookup"));

    let error = redeem_key(
        &fixture.identities,
        &code,
        "Second owner",
        "Second device",
        fingerprint("second-owner"),
    )
    .expect_err("an outstanding bootstrap code cannot mint a second owner");
    expects_code(error, "bootstrap_closed", "bootstrap closed on redemption");
    assert_eq!(fixture.count("principals"), 1, "no second principal");
    assert_eq!(fixture.count("devices"), 1, "no second device");
    assert_eq!(fixture.count("role_assignments"), 1);
    assert_eq!(
        code_rows(&fixture)[0].1,
        None,
        "the refused redemption did not consume the code"
    );
    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].action, "identity.owner.paired");
    assert_eq!(
        refusals[0].metadata_json,
        "{\"reason\":\"bootstrap_closed\"}"
    );

    // The only surviving owner is the locally granted one.
    assert!(
        fixture
            .identities
            .current_policy(Some(&existing))
            .expect("owner policy")
            .holds_builtin(BuiltinRole::Owner)
    );
    assert!(
        !fixture
            .identities
            .current_policy(None)
            .expect("anonymous policy")
            .has_current_identity()
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_bootstrap_code_is_single_use_and_only_its_digest_is_stored_or_audited() {
    let fixture = Fixture::open();
    let now = fixture.clock.millis();
    let nonce = nonce_for("single-use-code");
    let payload = attestation(
        &nonce,
        Purpose::OwnerBootstrap,
        "test-os-user",
        now,
        now + 60_000,
    );
    assert!(!format!("{payload:?}").contains(&nonce));
    let proof = fixture.proof_from(payload);
    assert_eq!(format!("{proof:?}"), "LocalProof[<redacted>]");

    let (code, expires_at_ms) = issue_code(
        &fixture,
        &proof,
        LocalCommand::Bootstrap(CodeLifetime::default()),
    )
    .expect("issue a bootstrap code");
    assert_eq!(expires_at_ms, INITIAL_TIME_MS + DEFAULT_CODE_TTL_MS);

    // The persisted row is a SHA-256 digest, never the secret.
    let rows = code_rows(&fixture);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, sha256_hex(&code));
    assert_ne!(rows[0].0, code);
    assert_eq!(rows[0].0.len(), 64);
    // The nonce is persisted only as a digest too.
    let digests = nonce_digests(&fixture);
    assert_eq!(
        digests,
        vec![sha256_hex(&format!("vesta-local-proof-v1:{nonce}"))]
    );
    assert!(!digests[0].contains(&nonce));

    let added = redeem_key(
        &fixture.identities,
        &code,
        "Owner",
        "Owner device",
        fingerprint("single-use-code"),
    )
    .expect("first redemption succeeds");
    assert_eq!(fixture.count("principals"), 1);
    assert_eq!(fixture.count("devices"), 1);
    assert!(fixture.identities.owner_exists().expect("owner lookup"));

    // The second redemption of the same code is refused and leaves no partial rows.
    let error = redeem_key(
        &fixture.identities,
        &code,
        "Again",
        "Again device",
        fingerprint("single-use-code-again"),
    )
    .expect_err("a bootstrap code is single use");
    expects_code(error, "code_invalid", "replayed code");
    assert_eq!(fixture.count("principals"), 1);
    assert_eq!(fixture.count("devices"), 1);
    assert_eq!(fixture.count("role_assignments"), 1);
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(&added)))
            .expect("owner policy")
            .holds_builtin(BuiltinRole::Owner)
    );

    // Neither the secret nor the nonce ever reaches the audit chain.
    let text = audit_text(&fixture);
    assert!(!text.contains(&code), "the code secret was audited");
    assert!(!text.contains(&nonce), "the proof nonce was audited");
    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].action, "identity.owner.paired");
    assert_eq!(refusals[0].metadata_json, "{\"reason\":\"code_invalid\"}");
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_garbage_aliased_or_non_canonical_code_never_pairs_an_owner() {
    let fixture = Fixture::open();
    let (code, _) = bootstrap_code(&fixture);
    let unissued = never_issued_code();
    let too_long = format!("{unissued}A");
    // A deterministic case variant of a canonical token: the uppercased spelling is no
    // longer canonical Base64URL, so it is refused rather than case-folded back.
    let unissued_upper = unissued.to_uppercase();
    let padded = STANDARD.encode([0x5a_u8; 20]);
    let wrong_alphabet = format!("/{code}");
    let wrong_alphabet = wrong_alphabet[..27].to_owned();
    // No Java `trim()`: surrounding whitespace is not stripped.
    let trailing_newline = format!("{code}\n");
    let leading_space = format!(" {code}");

    // Syntactically impossible tokens never reach the service at all. `unissued` itself
    // IS canonical, so it is deliberately excluded here and exercised below instead.
    assert!(CodeInput::parse(&unissued).is_ok());
    let impossible: Vec<(&str, &str)> = vec![
        ("garbage", "invalid-code"),
        ("empty", ""),
        ("too-short", "AAAA"),
        ("too-long", too_long.as_str()),
        ("padded-standard-alphabet", padded.as_str()),
        ("non-url-safe-alphabet", wrong_alphabet.as_str()),
        ("case-variant", unissued_upper.as_str()),
        ("trailing-newline", trailing_newline.as_str()),
        ("leading-space", leading_space.as_str()),
    ];
    for (label, value) in impossible {
        let error = CodeInput::parse(value).expect_err("a non-canonical code is rejected");
        assert_eq!(error.code(), "code_invalid", "{label}");
    }

    // A canonical token that differs from the issued code by exactly one character is a
    // different code. Only a non-final character is perturbed, so the alias stays
    // canonical Base64URL and the refusal must come from the service, not the parser.
    let mut alias_bytes = code.clone().into_bytes();
    alias_bytes[0] = if alias_bytes[0] == b'A' { b'B' } else { b'A' };
    let alias = String::from_utf8(alias_bytes).expect("Base64URL text is ASCII");
    assert_ne!(alias, code);
    assert!(
        CodeInput::parse(&alias).is_ok(),
        "a one-character variant of a canonical token is still canonical"
    );
    let error = redeem_key(
        &fixture.identities,
        &alias,
        "Alias",
        "Alias device",
        fingerprint("code-alias"),
    )
    .expect_err("an aliased code is not the issued code");
    expects_code(error, "code_invalid", "one-character alias");

    // So is a canonical token that was never issued at all.
    let error = redeem_key(
        &fixture.identities,
        &unissued,
        "Never",
        "Never device",
        fingerprint("never-issued-code"),
    )
    .expect_err("a never-issued code cannot pair");
    expects_code(error, "code_invalid", "never-issued code");

    assert_eq!(fixture.count("principals"), 0);
    assert_eq!(fixture.count("devices"), 0);
    assert_eq!(fixture.count("role_assignments"), 0);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(rejected_entries(&fixture).len(), 2);

    // The real code still works, so the alias attempts left nothing behind.
    redeem_key(
        &fixture.identities,
        &code,
        "Owner",
        "Owner device",
        fingerprint("code-alias"),
    )
    .expect("the issued code is unaffected");
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(fixture.count("principals"), 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn fingerprints_are_canonical_and_a_revoked_device_key_still_conflicts() {
    let fixture = Fixture::open();
    let (code, _) = bootstrap_code(&fixture);
    let uppercase = "AB".repeat(32);
    assert_eq!(uppercase.len(), 64);

    let owner = redeem_key(
        &fixture.identities,
        &code,
        "Owner",
        "Owner device",
        Fingerprint::parse(&uppercase).expect("uppercase hex is accepted and canonicalised"),
    )
    .expect("pair the owner");
    assert_eq!(
        device_fingerprint(&fixture, &subject_of(&owner)),
        uppercase.to_lowercase(),
        "the stored fingerprint is lowercased"
    );

    let viewer = fixture.subject(role(BuiltinRole::Viewer));
    let viewer_key = device_fingerprint(&fixture, &viewer);

    // Revoke the viewer device through the real remote command path.
    let change = fixture
        .identities
        .apply_remote(
            &subject_of(&owner),
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        )
        .expect("the owner revokes a device");
    assert!(matches!(change, AuthorityChange::Revoked));
    assert!(
        device_revoked(&fixture, &viewer),
        "the viewer device really is revoked"
    );

    // Re-enrolling that key conflicts even though the device is revoked, and an
    // uppercase spelling of the same key conflicts too.
    let spellings = [
        (
            "exact",
            Fingerprint::parse(&viewer_key).expect("the stored fingerprint is canonical"),
        ),
        (
            "uppercase",
            Fingerprint::parse(&viewer_key.to_uppercase()).expect("uppercase hex is accepted"),
        ),
    ];
    for (label, key) in spellings {
        let (recovery, _) = recovery_code(&fixture, RecoveryAction::KeepAccess);
        let error = redeem_key(
            &fixture.identities,
            &recovery,
            "Impostor",
            "Impostor device",
            key,
        )
        .expect_err("a known device key conflicts");
        assert_eq!(error.code(), "conflict", "{label} spelling of a known key");
    }

    assert_eq!(fixture.count("principals"), 2, "no impostor principal");
    assert_eq!(fixture.count("devices"), 2, "no impostor device");
    assert!(
        !fixture
            .identities
            .current_policy(Some(&viewer))
            .expect("viewer policy")
            .has_current_identity(),
        "a revoked device resolves to no authority"
    );
    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 2);
    for refusal in &refusals {
        assert_eq!(refusal.action, "identity.owner.paired");
        assert_eq!(refusal.metadata_json, "{\"reason\":\"conflict\"}");
    }
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn an_imported_uppercase_device_row_conflicts_and_cannot_be_re_enrolled_even_when_revoked() {
    let fixture = Fixture::open();
    let (code, _) = bootstrap_code(&fixture);
    let owner = redeem_key(
        &fixture.identities,
        &code,
        "Owner",
        "Owner device",
        fingerprint("imported-row-owner"),
    )
    .expect("pair the owner");
    let holder = fixture.subject(role(BuiltinRole::Viewer));

    // Trusted fixture import: a device row written directly, as an external tool might,
    // whose digest is stored with uppercase hex and which is already revoked. This is
    // not reachable through any production enrollment path, so it is inserted verbatim.
    let imported = "EF".repeat(32);
    assert_eq!(imported.len(), 64);
    let device_id = uuid::Uuid::new_v4().to_string();
    let now = fixture.clock.millis();
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at, revoked_at, revoked_by) \
                 VALUES (?,?,?,'Imported device',?,?,?,'import')",
                rusqlite::params![
                    device_id,
                    fixture.db.server_id(),
                    holder.principal.as_str(),
                    imported,
                    now,
                    now
                ],
            )?;
            Ok(())
        })
        .expect("insert a trusted fixture device row");

    // The canonical lowercase spelling of that key must conflict, even though the stored
    // row is uppercase and the device is revoked.
    let (recovery, _) = recovery_code(&fixture, RecoveryAction::KeepAccess);
    let error = redeem_key(
        &fixture.identities,
        &recovery,
        "Impostor",
        "Impostor device",
        Fingerprint::parse(&imported).expect("uppercase hex is canonicalised on the way in"),
    )
    .expect_err("an imported uppercase digest still conflicts");
    assert_eq!(error.code(), "conflict");
    // owner + imported-row holder, and the owner's/holder's/imported devices.
    assert_eq!(fixture.count("principals"), 2, "no impostor principal");
    assert_eq!(fixture.count("devices"), 3, "no impostor device");

    // A different key is unaffected, so the conflict is specific and not a blanket
    // refusal for this server.
    let (second, _) = recovery_code(&fixture, RecoveryAction::KeepAccess);
    redeem_key(
        &fixture.identities,
        &second,
        "Legitimate owner",
        "Second device",
        fingerprint("imported-row-legitimate"),
    )
    .expect("a distinct key still enrolls");
    assert_eq!(fixture.count("principals"), 3);
    assert_eq!(fixture.count("devices"), 4);
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(&owner)))
            .expect("owner policy")
            .holds_builtin(BuiltinRole::Owner)
    );
    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].action, "identity.owner.paired");
    assert_eq!(refusals[0].metadata_json, "{\"reason\":\"conflict\"}");
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// -- Java: codeRotationInvalidatesEarlierCodeAndExpiryRejectsAtExactDeadline

#[test]
fn issuing_a_new_code_invalidates_outstanding_codes_and_expiry_rejects_at_the_exact_deadline() {
    let fixture = Fixture::open();
    let (stale, _) = bootstrap_code(&fixture);
    let (live, expires_at_ms) = bootstrap_code_with(
        &fixture,
        CodeLifetime::new(MIN_CODE_LIFETIME).expect("60s code lifetime"),
    );
    assert_eq!(expires_at_ms, INITIAL_TIME_MS + 60_000);

    // Rotation invalidated the earlier code without deleting its row.
    let rows = code_rows(&fixture);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, None, "the stale code was never consumed");
    assert_eq!(
        rows[0].2,
        Some(INITIAL_TIME_MS),
        "the stale code was invalidated"
    );
    assert_eq!(rows[1].1, None);
    assert_eq!(rows[1].2, None);

    let error = redeem_key(
        &fixture.identities,
        &stale,
        "Owner",
        "Owner device",
        fingerprint("rotation-stale"),
    )
    .expect_err("an invalidated code cannot pair");
    expects_code(error, "code_invalid", "rotated-out code");
    assert_eq!(fixture.count("principals"), 0);

    // AT the exact expiry millisecond the code is already dead.
    fixture.clock.set(expires_at_ms);
    let error = redeem_key(
        &fixture.identities,
        &live,
        "Owner",
        "Owner device",
        fingerprint("rotation-exact-deadline"),
    )
    .expect_err("a code is refused at its exact deadline");
    expects_code(error, "code_invalid", "exact expiry deadline");
    assert_eq!(fixture.count("principals"), 0);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(
        code_rows(&fixture)[1].1,
        None,
        "the refused redemption did not consume the code"
    );

    // One millisecond earlier it is still live.
    fixture.clock.set(expires_at_ms - 1);
    let added = redeem_key(
        &fixture.identities,
        &live,
        "Owner",
        "Owner device",
        fingerprint("rotation-just-before-deadline"),
    )
    .expect("a code is live one millisecond before its deadline");
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(fixture.count("principals"), 1);
    assert_eq!(fixture.count("devices"), 1);
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(&added)))
            .expect("owner policy")
            .holds_builtin(BuiltinRole::Owner)
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// -- Java: concurrentRedemptionCreatesOnlyOneOwnerAndOneDevice

#[test]
fn concurrent_redemption_of_one_code_creates_at_most_one_owner_and_one_device() {
    const ATTEMPTS: usize = 8;

    let fixture = Fixture::open();
    let (code, _) = bootstrap_code(&fixture);
    // `Fixture` owns the database and the temp directory, so each thread receives a
    // cloned service handle and every thread is joined before the fixture can drop.
    let identities = fixture.identities.clone();
    let code = code.as_str();

    let results: Vec<Result<OwnerAdded>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..ATTEMPTS)
            .map(|index| {
                let identities = identities.clone();
                scope.spawn(move || {
                    redeem_key(
                        &identities,
                        code,
                        "Owner",
                        "Owner device",
                        fingerprint(&format!("race-key-{index}")),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(result) => result,
                Err(_) => panic!("a redemption thread panicked"),
            })
            .collect()
    });

    let successes: Vec<&OwnerAdded> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(successes.len(), 1, "exactly one racer may pair the owner");
    let losers: Vec<(usize, &ControlError)> = results
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| outcome.as_ref().err().map(|error| (index, error)))
        .collect();
    assert_eq!(losers.len(), ATTEMPTS - 1, "every other racer is refused");
    for (index, error) in losers {
        assert!(
            error.code() == "code_invalid" || error.code() == "bootstrap_closed",
            "racer {index} was refused with {}",
            error.code()
        );
    }

    assert_eq!(fixture.count("principals"), 1, "no extra principal");
    assert_eq!(fixture.count("devices"), 1, "no extra device");
    assert_eq!(
        fixture.count("role_assignments"),
        1,
        "no extra owner assignment"
    );
    assert_eq!(fixture.count("bootstrap_codes"), 1);
    assert_eq!(fixture.count("local_authority_uses"), 1);
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert_eq!(count_success(&fixture, "identity.owner.paired"), 1);
    assert_eq!(count_success(&fixture, "identity.bootstrap.issue"), 1);
    assert_eq!(
        rejected_entries(&fixture).len(),
        ATTEMPTS - 1,
        "every loser is audited"
    );
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(successes[0])))
            .expect("owner policy")
            .holds_builtin(BuiltinRole::Owner)
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// -- Java: localRecoveryRevokesExistingAdministrativeAccessAndPairsANewOwner

#[test]
fn recovery_revokes_administrative_access_and_pairs_a_new_owner() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let owner_policy = fixture
        .identities
        .current_policy(Some(&owner))
        .expect("owner policy");
    let admin_policy = fixture
        .identities
        .current_policy(Some(&admin))
        .expect("admin policy");
    assert_eq!(owner_policy.rank(), BuiltinRole::Owner.rank());
    assert_eq!(admin_policy.rank(), BuiltinRole::Administrator.rank());
    assert!(
        owner_policy
            .check(Capability::BackupsRestore, None)
            .allowed()
    );
    assert!(
        admin_policy
            .check(Capability::SecurityManage, None)
            .allowed()
    );

    let (code, _) = recovery_code(&fixture, RecoveryAction::RevokeAdministrativeAccess);

    // The Owner and the Administrator lose both their assignment and their device.
    for (label, subject) in [("owner", &owner), ("admin", &admin)] {
        let policy = fixture
            .identities
            .current_policy(Some(subject))
            .expect("policy after recovery");
        assert!(
            !policy.has_current_identity(),
            "{label} must hold no authority after recovery"
        );
        assert!(policy.roles().is_empty(), "{label} keeps no active role");
        assert_eq!(
            policy.check(Capability::SecurityManage, None),
            Decision::Deny(DenyReason::NoIdentity),
            "{label} resolves to no authority"
        );
        assert!(
            device_revoked(&fixture, subject),
            "{label} device is revoked"
        );
    }
    // The viewer is not administrative and is untouched.
    let viewer_policy = fixture
        .identities
        .current_policy(Some(&viewer))
        .expect("viewer policy");
    assert!(viewer_policy.has_current_identity());
    assert!(viewer_policy.holds_builtin(BuiltinRole::Viewer));
    assert!(!device_revoked(&fixture, &viewer), "viewer device survives");
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));

    let recovered = redeem_key(
        &fixture.identities,
        &code,
        "Recovered owner",
        "New device",
        fingerprint("recovered-owner"),
    )
    .expect("a recovery code pairs a new owner");
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(&recovered)))
            .expect("recovered policy")
            .check(Capability::BackupsRestore, None)
            .allowed()
    );
    assert_eq!(fixture.count("principals"), 4);
    assert_eq!(
        fixture.count("role_assignments"),
        4,
        "revocations keep their rows; only a new owner assignment is added"
    );
    assert_eq!(
        fixture.audit_actions(),
        vec![
            "identity.bootstrap.issue".to_owned(),
            "identity.owner.paired".to_owned(),
            "authority.role.assign".to_owned(),
            "authority.role.assign".to_owned(),
            "identity.recovery.issue".to_owned(),
            "identity.owner.paired".to_owned(),
        ]
    );
    let recovery = audit_entries(&fixture)
        .into_iter()
        .find(|entry| entry.action == "identity.recovery.issue")
        .expect("recovery audit");
    assert_eq!(recovery.outcome, "success");
    assert!(
        recovery
            .metadata_json
            .contains("\"revoke_admin_access\":\"true\"")
    );
    assert!(
        recovery
            .metadata_json
            .contains("\"revoked_assignments\":\"2\"")
    );
    assert!(recovery.metadata_json.contains("\"revoked_devices\":\"2\""));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn recovery_keeping_access_leaves_authority_intact_and_invalidates_outstanding_codes() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let admin = fixture.subject(role(BuiltinRole::Administrator));

    let (stale, _) = recovery_code(&fixture, RecoveryAction::KeepAccess);
    assert_eq!(
        code_rows(&fixture)[1].2,
        None,
        "the recovery code is outstanding"
    );

    let (live, _) = recovery_code(&fixture, RecoveryAction::KeepAccess);
    let rows = code_rows(&fixture);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[1].2,
        Some(INITIAL_TIME_MS),
        "issuing a recovery code invalidated the outstanding one"
    );
    assert_eq!(rows[2].2, None);

    // Outstanding codes are invalidated either way.
    let error = redeem_key(
        &fixture.identities,
        &stale,
        "Second",
        "Second device",
        fingerprint("keep-access-stale"),
    )
    .expect_err("the invalidated recovery code cannot pair");
    expects_code(error, "code_invalid", "code invalidated by recovery");

    for (label, subject, capability) in [
        ("owner", &owner, Capability::BackupsRestore),
        ("admin", &admin, Capability::SecurityManage),
    ] {
        let policy = fixture
            .identities
            .current_policy(Some(subject))
            .expect("policy after KeepAccess");
        assert!(policy.has_current_identity(), "{label} keeps its device");
        assert!(
            policy.check(capability, None).allowed(),
            "{label} keeps its administrative authority"
        );
        assert!(
            !device_revoked(&fixture, subject),
            "{label} device survives"
        );
    }
    assert!(fixture.identities.owner_exists().expect("owner lookup"));

    let added = redeem_key(
        &fixture.identities,
        &live,
        "Recovered owner",
        "New device",
        fingerprint("keep-access-recovered"),
    )
    .expect("a recovery code still pairs a new owner");
    assert!(
        fixture
            .identities
            .current_policy(Some(&subject_of(&added)))
            .expect("recovered policy")
            .check(Capability::BackupsRestore, None)
            .allowed()
    );
    // KeepAccess preserved the previous owner too.
    assert!(
        fixture
            .identities
            .current_policy(Some(&owner))
            .expect("original owner policy")
            .check(Capability::BackupsRestore, None)
            .allowed()
    );
    let recovery = audit_entries(&fixture)
        .into_iter()
        .find(|entry| entry.action == "identity.recovery.issue")
        .expect("recovery audit");
    assert!(
        recovery
            .metadata_json
            .contains("\"revoke_admin_access\":\"false\"")
    );
    assert!(
        recovery
            .metadata_json
            .contains("\"revoked_assignments\":\"0\"")
    );
    assert!(recovery.metadata_json.contains("\"revoked_devices\":\"0\""));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// -- Java: recoveryAuditFailurePreservesOldAdministrativeAccessAndTheLocalProofForRetry

#[test]
fn recovery_audit_failure_preserves_administrative_access_and_the_local_proof_for_retry() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let codes = fixture.count("bootstrap_codes");
    let nonces = fixture.count("local_authority_uses");
    let audit_rows = fixture.count("audit_events");

    let proof = fixture.proof(Purpose::OwnerRecovery);
    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_local(
            Some(&proof),
            recovery_command(RecoveryAction::RevokeAdministrativeAccess),
        )
        .expect_err("a failed recovery audit rolls the recovery back");
    expects_infrastructure_failure(error, "recovery audit failure");
    assert_eq!(fixture.count("bootstrap_codes"), codes, "no code row");
    assert_eq!(
        fixture.count("local_authority_uses"),
        nonces,
        "no nonce row"
    );
    assert_eq!(fixture.count("audit_events"), audit_rows, "no audit row");

    for (label, subject, capability) in [
        ("owner", &owner, Capability::BackupsRestore),
        ("admin", &admin, Capability::SecurityManage),
    ] {
        let policy = fixture
            .identities
            .current_policy(Some(subject))
            .expect("policy after the rolled-back recovery");
        assert!(policy.has_current_identity(), "{label} keeps its device");
        assert!(
            policy.check(capability, None).allowed(),
            "{label} keeps its administrative authority"
        );
        assert!(
            !device_revoked(&fixture, subject),
            "{label} device survives"
        );
    }
    assert!(fixture.identities.owner_exists().expect("owner lookup"));

    // The identical proof value is still unconsumed, so the retry succeeds.
    fixture.audit_failure(false);
    let (code, _) = issue_code(
        &fixture,
        &proof,
        recovery_command(RecoveryAction::RevokeAdministrativeAccess),
    )
    .expect("the rolled-back recovery is retryable with the same proof");
    assert_eq!(
        code_rows(&fixture).len() as i64,
        codes + 1,
        "the retry issued exactly one code"
    );
    for (label, subject) in [("owner", &owner), ("admin", &admin)] {
        assert!(
            !fixture
                .identities
                .current_policy(Some(subject))
                .expect("policy after recovery")
                .has_current_identity(),
            "{label} lost its authority only on the successful retry"
        );
    }
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        code_rows(&fixture)
            .iter()
            .any(|(digest, _, _)| *digest == sha256_hex(&code))
    );

    redeem_key(
        &fixture.identities,
        &code,
        "Recovered owner",
        "New device",
        fingerprint("recovery-audit-failure-recovered"),
    )
    .expect("the issued recovery code pairs a new owner");
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_refused_recovery_whose_audit_also_fails_reports_audit_unavailable() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let codes = fixture.count("bootstrap_codes");
    let nonces = fixture.count("local_authority_uses");
    let audit_rows = fixture.count("audit_events");
    let command = recovery_command(RecoveryAction::RevokeAdministrativeAccess);

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_local(None, command.clone())
        .expect_err("an absent proof is refused");
    expects_audit_unavailable(error, "local_authority_required");

    // A proof issued for the wrong purpose is refused the same way.
    let wrong_purpose = fixture.proof(Purpose::OwnerBootstrap);
    let error = fixture
        .identities
        .apply_local(Some(&wrong_purpose), command.clone())
        .expect_err("a wrong-purpose proof is refused");
    expects_audit_unavailable(error, "local_authority_required");

    assert_eq!(
        fixture.count("bootstrap_codes"),
        codes,
        "authority unchanged"
    );
    assert_eq!(fixture.count("local_authority_uses"), nonces);
    assert_eq!(fixture.count("audit_events"), audit_rows);
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        fixture
            .identities
            .current_policy(Some(&owner))
            .expect("owner policy")
            .check(Capability::BackupsRestore, None)
            .allowed()
    );

    // With the fault removed the very same refusal is recorded normally, and a valid
    // recovery proof then revokes the old administrative access.
    fixture.audit_failure(false);
    let error = fixture
        .identities
        .apply_local(None, command)
        .expect_err("the proof is still absent");
    expects_code(error, "local_authority_required", "absent recovery proof");
    let refusals = rejected_entries(&fixture);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].action, "identity.recovery.issue");
    assert_eq!(
        refusals[0].metadata_json,
        "{\"reason\":\"local_authority_required\"}"
    );

    recovery_code(&fixture, RecoveryAction::RevokeAdministrativeAccess);
    assert!(!fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        !fixture
            .identities
            .current_policy(Some(&owner))
            .expect("owner policy")
            .has_current_identity()
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}
