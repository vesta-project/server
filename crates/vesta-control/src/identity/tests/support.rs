//! Unit-test-only fixtures over actual protected SQLite state, clocks, audits and
//! the production identity APIs. No production feature, fake service or audit shim.
pub(super) use super::super::Subject;
pub(super) use super::super::local_authority::Attestation;
use super::super::local_authority::LocalAuthority;
use super::super::{
    AssignmentId, Capability, CodeInput, DeviceId, DeviceLabel, DisplayName, Fingerprint,
    IdentityService, LocalCommand, PrincipalId, Purpose, RedeemOwner, RoleId,
};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tempfile::TempDir;
use vesta_state::audit::AuditLog;
use vesta_state::{Clock, StateDatabase, StateDirectory};

pub(super) const INITIAL_TIME_MS: i64 = 1_791_115_200_000; // 2026-10-04T12:00:00Z

pub(super) struct TestClock {
    now: AtomicI64,
}

impl TestClock {
    pub(super) fn new(ms: i64) -> Self {
        Self {
            now: AtomicI64::new(ms),
        }
    }

    pub(super) fn set(&self, ms: i64) {
        self.now.store(ms, Ordering::SeqCst);
    }

    pub(super) fn advance(&self, delta_ms: i64) {
        self.now
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                now.checked_add(delta_ms)
            })
            .expect("test clock arithmetic overflow");
    }
}

impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

struct TestAuthority;

impl LocalAuthority for TestAuthority {
    fn verify(&self, proof: &super::super::LocalProof, _expected: Purpose) -> Option<Attestation> {
        // As in the Java TestProof fixture: the production service must enforce
        // purpose and every temporal/nonce/actor check, not the test authority.
        proof.attestation_for_test()
    }
}

pub(super) struct Fixture {
    pub(super) root: TempDir,
    pub(super) clock: Arc<TestClock>,
    pub(super) db: StateDatabase,
    pub(super) audit: AuditLog,
    pub(super) identities: IdentityService,
}

impl Fixture {
    pub(super) fn open() -> Self {
        Self::with_time(INITIAL_TIME_MS)
    }

    pub(super) fn with_time(ms: i64) -> Self {
        let root = TempDir::new().expect("create identity test root");
        let clock = Arc::new(TestClock::new(ms));
        let directory = StateDirectory::new(root.path().join("state"), None)
            .expect("identity test state directory");
        let db =
            StateDatabase::open(directory, clock.clone()).expect("open identity test database");
        let audit = AuditLog::new(db.clone()).expect("verify identity test audit");
        let identities =
            IdentityService::with_test_local_authority(db.clone(), Arc::new(TestAuthority))
                .expect("create test-only identity authority");
        Self {
            root,
            clock,
            db,
            audit,
            identities,
        }
    }

    pub(super) fn proof(&self, purpose: Purpose) -> super::super::LocalProof {
        let issued_at_ms = self.clock.millis();
        self.proof_from(Attestation {
            nonce: uuid::Uuid::new_v4().to_string(),
            purpose,
            os_actor: "test-os-user".into(),
            issued_at_ms,
            expires_at_ms: issued_at_ms
                .checked_add(60_000)
                .expect("test proof expiry overflow"),
        })
    }

    pub(super) fn proof_from(&self, payload: Attestation) -> super::super::LocalProof {
        super::super::LocalProof::for_test(payload)
    }

    /// Evidence the production service must refuse: opaque, correctly shaped, but
    /// issued by no authority.
    pub(super) fn foreign_proof(&self) -> super::super::LocalProof {
        super::super::LocalProof::foreign_for_test()
    }

    pub(super) fn bootstrap_code(&self) -> String {
        let proof = self.proof(Purpose::OwnerBootstrap);
        let outcome = self
            .identities
            .apply_local(Some(&proof), LocalCommand::Bootstrap(Default::default()))
            .expect("issue fixture owner bootstrap code");
        match outcome {
            super::super::LocalOutcome::Code(code) => code.expose_secret().to_owned(),
            other => panic!("bootstrap must return a code outcome, got {other:?}"),
        }
    }

    /// Redeems through the production single-use code path, so a fixture owner is a
    /// real Owner assignment with a real success audit record.
    pub(super) fn owner(&self) -> Subject {
        let code = self.bootstrap_code();
        self.redeem(&code, "Owner", "Owner device")
    }
    pub(super) fn redeem(&self, code: &str, name: &str, device: &str) -> Subject {
        let added = self
            .identities
            .redeem_owner(RedeemOwner {
                code: CodeInput::parse(code).expect("fixture code is canonical"),
                name: DisplayName::parse(name).expect("fixture display name"),
                device_label: DeviceLabel::parse(device).expect("fixture device label"),
                fingerprint: fingerprint(&uuid::Uuid::new_v4().to_string()),
            })
            .expect("pair fixture owner");
        Subject {
            principal: added.principal,
            device: added.device,
        }
    }

    /// Trusted fixture insertion, NOT a production enrollment endpoint. Its role is
    /// still assigned through the actual local-proof mutation/audit path.
    pub(super) fn subject(&self, role: RoleId) -> Subject {
        let principal =
            PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).expect("fixture principal id");
        let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("fixture device id");
        let now = self.clock.millis();
        self.db
            .write(|connection| {
                connection.execute(
                    "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) \
                     VALUES (?,?,'Fixture','active',?,?)",
                    params![principal.as_str(), self.db.server_id(), now, now],
                )?;
                connection.execute(
                    "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at) \
                     VALUES (?,?,?,?,?,?)",
                    params![
                        device.as_str(),
                        self.db.server_id(),
                        principal.as_str(),
                        "Fixture device",
                        fingerprint(&uuid::Uuid::new_v4().to_string()).as_str(),
                        now
                    ],
                )?;
                Ok(())
            })
            .expect("insert trusted fixture subject");
        let proof = self.proof(Purpose::LocalRoleGrant);
        self.identities
            .apply_local(
                Some(&proof),
                LocalCommand::AssignRole {
                    principal: principal.clone(),
                    role,
                    expiry: super::super::Expiry::Never,
                },
            )
            .expect("assign fixture subject role");
        Subject { principal, device }
    }

    /// Deliberately accepts malformed stored scope text for fail-closed tests.
    pub(super) fn stored_override(
        &self,
        subject: &Subject,
        capability: Capability,
        scope: &str,
        effect: super::super::Effect,
        expiry: Option<i64>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.db
            .write(|connection| {
                connection.execute(
                    "INSERT INTO principal_overrides (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at) \
                     VALUES (?,?,?,?,?,'fixture',?,?)",
                    params![
                        id,
                        subject.principal.as_str(),
                        capability.id(),
                        scope,
                        effect.stored(),
                        self.clock.millis(),
                        expiry
                    ],
                )?;
                Ok(())
            })
            .expect("insert trusted fixture override");
        id
    }

    pub(super) fn count(&self, table: &str) -> i64 {
        assert!(
            matches!(
                table,
                "principals"
                    | "devices"
                    | "roles"
                    | "role_capabilities"
                    | "role_assignments"
                    | "principal_overrides"
                    | "bootstrap_codes"
                    | "local_authority_uses"
                    | "audit_events"
            ),
            "count requires a known identity fixture table"
        );
        self.db
            .read(|connection| {
                Ok(
                    connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .expect("count fixture rows")
    }

    pub(super) fn sql(&self, statement: &str) {
        self.db
            .write(|connection| {
                connection.execute_batch(statement)?;
                Ok(())
            })
            .expect("execute fixture SQL")
    }

    pub(super) fn audit_failure(&self, enabled: bool) {
        self.sql(if enabled {
            "CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'injected'); END"
        } else {
            "DROP TRIGGER audit_failure"
        });
    }

    /// Audit rows for one action, newest last, without interpreting any text.
    pub(super) fn audit_actions(&self) -> Vec<String> {
        self.db
            .read(|connection| {
                let mut statement =
                    connection.prepare("SELECT action FROM audit_events ORDER BY seq")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .expect("read fixture audit actions")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // TempDir is disposed only after this Drop body. Close the shared anchor
        // and drain all actual DB users first (important for Windows WAL files).
        self.db.close().expect("close identity fixture database");
    }
}

pub(super) fn fingerprint(value: &str) -> Fingerprint {
    Fingerprint::parse(&hex::encode(Sha256::digest(value.as_bytes())))
        .expect("fixture fingerprint is canonical")
}

pub(super) fn parse_principal(id: &str) -> PrincipalId {
    PrincipalId::parse(id).expect("fixture principal id")
}

pub(super) fn assignment(id: AssignmentId) -> String {
    id.as_str().to_owned()
}
