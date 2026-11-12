//! Protected SQLite/policy primitives plus actual public confirmation orchestration.
//! Public-flow tests use the runner author's trusted owned inert native child and
//! its actual published generation. No service, provider, fake handler, proof
//! issuer or injected revision view. Native dispatch stops after real inert Start;
//! confirmation tests then assert queue/receipt/audit/event facts without kill.
use super::*;
use crate::identity::{Capability, DeviceId, PrincipalId};
use rusqlite::params;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tempfile::TempDir;
use vesta_game_runtime::process::GenerationId;
use vesta_state::{Clock, StateDatabase, StateDirectory};

const INITIAL_MS: i64 = 1_791_115_200_000;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    _root: TempDir,
    clock: Arc<TestClock>,
    db: StateDatabase,
    operations: Operations,
    subject: Subject,
    id: OperationId,
    generation: GenerationId,
}
impl Fixture {
    fn open() -> Self {
        let root = tempfile::tempdir().expect("confirmation test directory");
        let clock = Arc::new(TestClock(AtomicI64::new(INITIAL_MS)));
        let directory = StateDirectory::new(root.path().join("private state"), None)
            .expect("protect confirmation test state");
        let db = StateDatabase::open(directory, clock.clone()).expect("real confirmation database");
        let subject = Subject {
            principal: PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
            device: DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        };
        db.write(|connection| {
            connection.execute(
                "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) \
                 VALUES (?1,?2,'Confirmation fixture','active',?3,?3)",
                params![subject.principal.as_str(), db.server_id(), INITIAL_MS],
            )?;
            connection.execute(
                "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) \
                 VALUES (?1,?2,?3,'Confirmation fixture',?4,?5)",
                params![subject.device.as_str(), db.server_id(), subject.principal.as_str(),
                    hex::encode(Sha256::digest(subject.device.as_str().as_bytes())), INITIAL_MS],
            )?;
            connection.execute(
                "INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) \
                 VALUES (?1,?2,'owner','fixture',?3)",
                params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), INITIAL_MS],
            )?;
            Ok(())
        }).expect("trusted identity fixture facts");
        let operations = Operations::new(db.clone()).unwrap();
        let generation = GenerationId::parse_hex("0102030405060708090a0b0c0d0e0f10").unwrap();
        let command = GameCommand::ForceKill {
            expected_generation: generation,
        };
        let admission = operations
            .admit(
                &subject,
                "game.force_kill",
                &command.canonical_input().unwrap(),
                None,
            )
            .expect("admit real canonical game binding without execution");
        let id = match admission {
            super::super::Admission::Created(summary) => summary.id,
            _ => panic!("fresh fixture admission"),
        };
        Self {
            _root: root,
            clock,
            db,
            operations,
            subject,
            id,
            generation,
        }
    }
    fn now(&self) -> i64 {
        self.clock.millis()
    }
    fn revision(&self) -> SourceRevision {
        SourceRevision::for_game(Some(self.generation))
    }
    fn write<T>(
        &self,
        work: impl FnOnce(&Repository<'_>, &ConfirmationRepository<'_>) -> OperationsResult<T>,
    ) -> OperationsResult<T> {
        self.db
            .write_with(|transaction| {
                let repo =
                    Repository::new(transaction.connection(), self.db.server_id(), self.now());
                let confirmations =
                    ConfirmationRepository::new(repo.connection(), self.db.server_id(), self.now());
                work(&repo, &confirmations)
            })
            .map_err(OperationError::from)
    }
    /// Calls private SQL primitives with canonical server-owned test facts; this
    /// does not introduce a production alternate issuer or game-view provider.
    fn issue(&self) -> ConfirmationToken {
        let token = ConfirmationToken::random().unwrap();
        let hash = token.hash();
        self.write(|repo, confirmations| {
            let (operation, plan) =
                require_awaiting(&self.operations, repo, &self.subject, &self.id)?;
            require_current_target(&plan, &self.revision())?;
            confirmations.issue(
                &operation,
                &self.revision(),
                &hash,
                self.now() + MAX_CONFIRMATION_TTL_MS,
            )?;
            Ok(())
        })
        .expect("issue canonical confirmation SQL primitive");
        token
    }
    fn accept(&self, token: &ConfirmationToken) -> OperationsResult<()> {
        let hash = token.hash();
        self.write(|repo, confirmations| {
            let ticket = confirmations.burn(&hash)?.ok_or_else(rejected)?;
            let (operation, plan) =
                require_awaiting(&self.operations, repo, &self.subject, &self.id)?;
            require_current_target(&plan, &self.revision())?;
            confirmations.validate_ticket(&ticket, &operation, &self.revision())?;
            confirmations.accept(&ticket, &operation, &self.revision())?;
            confirmations.queue(&operation)?;
            let after = repo.require(&self.id)?;
            self.operations.record_state(
                repo,
                request_actor(&self.subject)?,
                "confirmation.consume",
                &after,
                OperationState::AwaitingConfirmation,
            )?;
            Ok(())
        })
    }
    fn times(&self, token: &ConfirmationToken) -> (Option<i64>, Option<i64>) {
        self.write(|_, confirmations| confirmations.stored_times(&token.hash()))
            .unwrap()
            .expect("confirmation row")
    }
    fn summary(&self) -> OperationSummary {
        self.operations
            .get(&self.subject, &self.id)
            .unwrap()
            .unwrap()
    }
    fn audit_failure(&self) {
        self.db
            .write(|connection| {
                connection.execute_batch(
                    "CREATE TRIGGER confirmation_audit_failure BEFORE INSERT ON audit_events \
                 BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END",
                )?;
                Ok(())
            })
            .unwrap();
    }
    fn deny(&self) {
        self.db.write(|connection| {
            connection.execute(
                "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) \
                 VALUES (?1,?2,?3,'*','deny','fixture',?4)",
                params![uuid::Uuid::new_v4().to_string(), self.subject.principal.as_str(),
                    Capability::LifecycleForceKill.id(), self.now()],
            )?;
            Ok(())
        }).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let result = self.db.close();
        if !std::thread::panicking() {
            result.expect("close actual confirmation database");
        }
    }
}

fn assert_refused(result: OperationsResult<()>) -> OperationError {
    let error = result.expect_err("confirmation must refuse");
    assert_eq!(error.code(), "confirmation_rejected");
    error
}

#[test]
fn token_owns_exact_allocation_is_bounded_and_redacted() {
    let text = "aB09-_owned-token".to_owned();
    let pointer = text.as_ptr();
    let token = ConfirmationToken::try_from(text).unwrap();
    assert_eq!(token.expose_secret(), "aB09-_owned-token");
    assert_eq!(token.expose_secret().as_ptr(), pointer);
    assert_eq!(format!("{token:?}"), "ConfirmationToken(<redacted>)");
    for input in [
        Vec::new(),
        vec![b'a'; MAX_TOKEN_BYTES + 1],
        vec![0xff],
        b"not a token".to_vec(),
        b"abc\0def".to_vec(),
    ] {
        let error = ConfirmationToken::try_from(input).unwrap_err();
        assert_eq!(error.code(), "invalid_request");
        assert_eq!(error.to_string(), "Operation request failed");
    }
    assert!(ConfirmationToken::try_from(vec![b'a'; MAX_TOKEN_BYTES]).is_ok());
}

#[test]
fn random_token_has_canonical_256_bit_encoding_and_persistence_stores_only_hash() {
    let fixture = Fixture::open();
    let first = fixture.issue();
    let second = fixture.issue();
    assert_ne!(first.expose_secret(), second.expose_secret());
    assert_eq!(second.expose_secret().len(), 43);
    let bytes = Zeroizing::new(URL_SAFE_NO_PAD.decode(second.expose_secret()).unwrap());
    assert_eq!(bytes.len(), 32);
    assert_eq!(
        URL_SAFE_NO_PAD.encode(bytes.as_slice()),
        second.expose_secret()
    );
    let stored: String = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT token_hash FROM confirmations ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(stored, *second.hash());
    assert_ne!(stored, second.expose_secret());
    let issued = IssuedConfirmation {
        confirmation_id: "fixture".to_owned(),
        operation_id: fixture.id.clone(),
        expires_at_ms: fixture.now() + 1,
        token: second,
    };
    assert!(!format!("{issued:?}").contains(issued.token.expose_secret()));
}

#[test]
fn ttl_bounds_are_millisecond_native_and_overflow_fails_closed() {
    assert!(valid_ttl_ms(Duration::ZERO).is_err());
    assert!(valid_ttl_ms(Duration::from_nanos(1)).is_err());
    assert_eq!(valid_ttl_ms(Duration::from_millis(1)).unwrap(), 1);
    assert_eq!(
        valid_ttl_ms(MAX_CONFIRMATION_TTL).unwrap(),
        MAX_CONFIRMATION_TTL_MS
    );
    assert!(valid_ttl_ms(MAX_CONFIRMATION_TTL + Duration::from_nanos(1)).is_err());
    assert!(
        i64::MAX
            .checked_add(valid_ttl_ms(Duration::from_millis(1)).unwrap())
            .is_none()
    );
}

#[test]
fn reissue_supersedes_prior_token_and_replay_never_reopens_receipt() {
    let fixture = Fixture::open();
    let first = fixture.issue();
    let second = fixture.issue();
    assert_eq!(fixture.times(&first), (Some(INITIAL_MS), None));
    fixture
        .write(|_, confirmations| {
            assert!(confirmations.burn(&first.hash())?.is_none());
            Ok(())
        })
        .unwrap();
    fixture.accept(&second).unwrap();
    assert_eq!(fixture.summary().state, OperationState::Queued);
    assert_eq!(fixture.times(&second), (Some(INITIAL_MS), Some(INITIAL_MS)));
    fixture
        .write(|repo, confirmations| {
            assert!(confirmations.burn(&second.hash())?.is_none());
            verify_receipt_in(
                repo.connection(),
                fixture.db.server_id(),
                &repo.require(&fixture.id)?,
                &fixture.revision(),
                fixture.now(),
            )
        })
        .unwrap();
}

#[test]
fn expired_token_is_burned_and_denial_audit_commits_without_acceptance() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    fixture
        .clock
        .0
        .store(INITIAL_MS + MAX_CONFIRMATION_TTL_MS, Ordering::SeqCst);
    let decision = fixture
        .write(|repo, confirmations| {
            let ticket = confirmations
                .burn(&token.hash())?
                .expect("live even though expired");
            let operation = repo.require(&fixture.id)?;
            let error = assert_refused(confirmations.validate_ticket(
                &ticket,
                &operation,
                &fixture.revision(),
            ));
            audit_refusal_in(
                &fixture.operations,
                repo.connection(),
                &fixture.subject,
                &fixture.id,
                &error,
            )?;
            Ok(ConfirmationDecision::Refused(error))
        })
        .unwrap();
    assert!(matches!(decision, ConfirmationDecision::Refused(_)));
    assert_eq!(fixture.times(&token), (Some(fixture.now()), None));
    assert_eq!(
        fixture.summary().state,
        OperationState::AwaitingConfirmation
    );
    fixture
        .write(|repo, _| {
            assert_refused(verify_receipt_in(
                repo.connection(),
                fixture.db.server_id(),
                &repo.require(&fixture.id)?,
                &fixture.revision(),
                fixture.now(),
            ));
            Ok(())
        })
        .unwrap();
}

#[test]
fn complete_binding_mismatches_burn_before_audit_and_never_make_receipt() {
    // Each case uses a separately issued token. Test facts may be modified only in
    // memory; production callers cannot supply these binding headers.
    for case in 0..7 {
        let fixture = Fixture::open();
        let token = fixture.issue();
        fixture
            .write(|repo, confirmations| {
                let ticket = confirmations.burn(&token.hash())?.unwrap();
                let mut operation = repo.require(&fixture.id)?;
                let mut revision = fixture.revision();
                match case {
                    0 => operation.summary.id = OperationId::parse("another-operation").unwrap(),
                    1 => {
                        operation.subject.principal =
                            PrincipalId::parse("another-principal").unwrap()
                    }
                    2 => operation.subject.device = DeviceId::parse("another-device").unwrap(),
                    3 => operation.operation_type = "game.stop".to_owned(),
                    4 => operation.target = "game:other".to_owned(),
                    5 => operation.payload_hash = "a".repeat(64),
                    6 => revision = SourceRevision::for_game(None),
                    _ => unreachable!(),
                }
                let error =
                    assert_refused(confirmations.validate_ticket(&ticket, &operation, &revision));
                audit_refusal_in(
                    &fixture.operations,
                    repo.connection(),
                    &fixture.subject,
                    &fixture.id,
                    &error,
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(fixture.times(&token), (Some(fixture.now()), None));
        assert_eq!(
            fixture.summary().state,
            OperationState::AwaitingConfirmation
        );
    }
}

#[test]
fn unknown_token_and_wrong_server_have_no_receipt_authority() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    let unknown = ConfirmationToken::try_from("unknown".to_owned()).unwrap();
    fixture
        .write(|repo, confirmations| {
            assert!(confirmations.burn(&unknown.hash())?.is_none());
            let other_server =
                ConfirmationRepository::new(repo.connection(), "another-server", fixture.now());
            assert!(other_server.burn(&token.hash())?.is_none());
            assert_refused(verify_receipt_in(
                repo.connection(),
                "another-server",
                &repo.require(&fixture.id)?,
                &fixture.revision(),
                fixture.now(),
            ));
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.times(&token), (None, None));
}

#[test]
fn current_policy_is_required_before_issue_and_consume_and_again_after_acceptance() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    fixture.deny();
    fixture
        .write(|repo, confirmations| {
            let issue_error =
                match require_awaiting(&fixture.operations, repo, &fixture.subject, &fixture.id) {
                    Ok(_) => panic!("revoked issue must refuse"),
                    Err(error) => error,
                };
            assert_eq!(issue_error.code(), "forbidden");
            assert!(confirmations.burn(&token.hash())?.is_some());
            let consume_error =
                match require_awaiting(&fixture.operations, repo, &fixture.subject, &fixture.id) {
                    Ok(_) => panic!("revoked consume must refuse"),
                    Err(error) => error,
                };
            audit_refusal_in(
                &fixture.operations,
                repo.connection(),
                &fixture.subject,
                &fixture.id,
                &consume_error,
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.times(&token), (Some(fixture.now()), None));

    let accepted = Fixture::open();
    let token = accepted.issue();
    accepted.accept(&token).unwrap();
    accepted.deny();
    accepted
        .write(|repo, _| {
            let operation = repo.require(&accepted.id)?;
            // A genuine receipt is not policy authority. This is the same canonical
            // policy prerequisite the kernel repeats before its receipt verifier.
            let error = match accepted.operations.verify_binding(repo, &operation) {
                Ok(_) => panic!("revoked execution must refuse"),
                Err(error) => error,
            };
            assert_eq!(error.code(), "forbidden");
            Ok(())
        })
        .unwrap();
}

#[test]
fn consumed_refused_and_legacy_rows_are_not_receipts_but_accepted_receipt_outlives_token_ttl() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    fixture
        .write(|repo, confirmations| {
            assert!(confirmations.burn(&token.hash())?.is_some());
            assert_refused(verify_receipt_in(
                repo.connection(),
                fixture.db.server_id(),
                &repo.require(&fixture.id)?,
                &fixture.revision(),
                fixture.now(),
            ));
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.times(&token).1, None); // v6 NULL also covers migrated legacy rows.

    let accepted = Fixture::open();
    let token = accepted.issue();
    accepted.accept(&token).unwrap();
    accepted
        .clock
        .0
        .store(INITIAL_MS + MAX_CONFIRMATION_TTL_MS + 1, Ordering::SeqCst);
    accepted
        .write(|repo, _| {
            let operation = repo.require(&accepted.id)?;
            verify_receipt_in(
                repo.connection(),
                accepted.db.server_id(),
                &operation,
                &accepted.revision(),
                accepted.now(),
            )?;
            assert_refused(verify_receipt_in(
                repo.connection(),
                accepted.db.server_id(),
                &operation,
                &SourceRevision::for_game(None),
                accepted.now(),
            ));
            assert_refused(verify_receipt_in(
                repo.connection(),
                accepted.db.server_id(),
                &operation,
                &accepted.revision(),
                INITIAL_MS - 1,
            ));
            Ok(())
        })
        .unwrap();
}

#[test]
fn success_audit_failure_rolls_back_burn_receipt_queue_and_event() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    let before: (i64, i64) = fixture
        .db
        .read(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM audit_events", [], |row| row.get(0))?,
                connection.query_row("SELECT count(*) FROM events", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    fixture.audit_failure();
    let error = fixture
        .accept(&token)
        .expect_err("acceptance audit must fail");
    assert!(matches!(error, OperationError::State(_)));
    assert_eq!(fixture.times(&token), (None, None));
    assert_eq!(
        fixture.summary().state,
        OperationState::AwaitingConfirmation
    );
    let after: (i64, i64) = fixture
        .db
        .read(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM audit_events", [], |row| row.get(0))?,
                connection.query_row("SELECT count(*) FROM events", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn refusal_audit_failure_rolls_back_burn_and_retains_primary_failure() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    fixture.audit_failure();
    let error = fixture
        .write(|repo, confirmations| {
            assert!(confirmations.burn(&token.hash())?.is_some());
            audit_refusal_in(
                &fixture.operations,
                repo.connection(),
                &fixture.subject,
                &fixture.id,
                &rejected(),
            )
        })
        .expect_err("denial audit must fail");
    assert_eq!(error.code(), "audit_unavailable");
    assert_eq!(error.primary().code(), "confirmation_rejected");
    assert_eq!(fixture.times(&token), (None, None));
    assert_eq!(
        fixture.summary().state,
        OperationState::AwaitingConfirmation
    );
    assert!(!format!("{error:?}").contains(token.expose_secret()));
    assert!(!format!("{error:?}").contains(fixture.revision().as_str()));
}

#[test]
fn oversized_confirmation_headers_are_compared_without_materializing_or_leaking_them() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE confirmations SET source_revision=?1 WHERE token_hash=?2",
                params!["private-revision".repeat(20_000), &*token.hash()],
            )?;
            Ok(())
        })
        .unwrap();
    fixture
        .write(|repo, confirmations| {
            let ticket = confirmations.burn(&token.hash())?.unwrap();
            let error = assert_refused(confirmations.validate_ticket(
                &ticket,
                &repo.require(&fixture.id)?,
                &fixture.revision(),
            ));
            assert!(!format!("{error:?}").contains("private-revision"));
            audit_refusal_in(
                &fixture.operations,
                repo.connection(),
                &fixture.subject,
                &fixture.id,
                &error,
            )?;
            Ok(())
        })
        .unwrap();
    let metadata: String = fixture.db.read(|connection| Ok(connection.query_row(
        "SELECT metadata_json FROM audit_events WHERE action='confirmation.consume' ORDER BY seq DESC LIMIT 1",
        [], |row| row.get(0),
    )?)).unwrap();
    assert!(!metadata.contains("private-revision"));
    assert!(!metadata.contains(token.expose_secret()));
    assert!(!metadata.contains(fixture.revision().as_str()));
}

#[test]
fn destructive_target_requires_exact_generation_not_just_a_well_formed_revision() {
    let fixture = Fixture::open();
    fixture
        .write(|repo, _| {
            let (_, plan) =
                require_awaiting(&fixture.operations, repo, &fixture.subject, &fixture.id)?;
            require_current_target(&plan, &fixture.revision())?;
            let error = require_current_target(&plan, &SourceRevision::for_game(None)).unwrap_err();
            assert_eq!(error.code(), "source_revision_unavailable");
            let other = GenerationId::parse_hex("1112131415161718191a1b1c1d1e1f20").unwrap();
            assert!(require_current_target(&plan, &SourceRevision::for_game(Some(other))).is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn awaiting_gate_requires_the_exact_principal_and_device_before_authority() {
    let fixture = Fixture::open();
    for subject in [
        Subject {
            principal: PrincipalId::parse("another-principal").unwrap(),
            device: fixture.subject.device.clone(),
        },
        Subject {
            principal: fixture.subject.principal.clone(),
            device: DeviceId::parse("another-device").unwrap(),
        },
    ] {
        fixture
            .write(|repo, _| {
                let error = match require_awaiting(&fixture.operations, repo, &subject, &fixture.id)
                {
                    Ok(_) => panic!("foreign actor/device cannot issue or consume"),
                    Err(error) => error,
                };
                assert_eq!(error.code(), "confirmation_rejected");
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn accepted_receipt_rechecks_every_header_and_rejects_ambiguous_acceptance() {
    let fixture = Fixture::open();
    let token = fixture.issue();
    fixture.accept(&token).unwrap();
    for case in 0..6 {
        fixture
            .write(|repo, _| {
                let mut operation = repo.require(&fixture.id)?;
                match case {
                    0 => operation.summary.id = OperationId::parse("another-operation").unwrap(),
                    1 => {
                        operation.subject.principal =
                            PrincipalId::parse("another-principal").unwrap()
                    }
                    2 => operation.subject.device = DeviceId::parse("another-device").unwrap(),
                    3 => operation.operation_type = "game.stop".to_owned(),
                    4 => operation.target = "game:other".to_owned(),
                    5 => operation.payload_hash = "b".repeat(64),
                    _ => unreachable!(),
                }
                assert_refused(verify_receipt_in(
                    repo.connection(),
                    fixture.db.server_id(),
                    &operation,
                    &fixture.revision(),
                    fixture.now(),
                ));
                Ok(())
            })
            .unwrap();
    }
    fixture.db.write(|connection| {
        connection.execute(
            "INSERT INTO confirmations (id,server_id,operation_id,actor_principal_id,device_id,action,\
             target,payload_hash,source_revision,token_hash,created_at,expires_at,consumed_at,accepted_at) \
             SELECT 'ambiguous-receipt',server_id,operation_id,actor_principal_id,device_id,action,\
             target,payload_hash,source_revision,?1,created_at,expires_at,consumed_at,accepted_at \
             FROM confirmations WHERE token_hash=?2",
            params!["c".repeat(64), &*token.hash()],
        )?;
        Ok(())
    }).unwrap();
    fixture
        .write(|repo, _| {
            assert_refused(verify_receipt_in(
                repo.connection(),
                fixture.db.server_id(),
                &repo.require(&fixture.id)?,
                &fixture.revision(),
                fixture.now(),
            ));
            Ok(())
        })
        .unwrap();
}

// Actual public gateway coverage. Reuses the runner author's agreed trusted
// inert-child entrypoint, without touching its tests or creating another handler.
// The actual runtime publishes its observed native generation; no fabricated view
// or revision-provider injection exists. Dispatch is stopped AFTER real Start so
// receipt/queue assertions cannot race an autonomous native ForceKill effect.
const PUBLIC_WAIT: Duration = Duration::from_secs(15);

struct OwnedPublicRuntime {
    runtime: OperationRuntime,
    cwd: std::path::PathBuf,
}
impl Drop for OwnedPublicRuntime {
    fn drop(&mut self) {
        // Test-only inert-child escape must precede observation-only runtime Drop,
        // including assertion unwinding. This is not a production process API.
        let _ = std::fs::write(self.cwd.join("release-game"), b"release");
        let result = self.runtime.shutdown(PUBLIC_WAIT);
        if !std::thread::panicking() {
            assert_eq!(
                result.expect("join actual inert native ownership"),
                super::super::runner::ShutdownOutcome::Complete
            );
        }
    }
}
struct PublicHarness {
    // Field order retains root/state until runtime has joined every native worker.
    owned: OwnedPublicRuntime,
    fixture: Fixture,
    id: OperationId,
    generation: GenerationId,
}
impl PublicHarness {
    fn open() -> Self {
        let fixture = Fixture::open();
        // The pure-domain fixture's synthetic lookup-key operation is never run.
        // Remove it from nonterminal work before opening real native ownership.
        fixture
            .operations
            .request_cancel(&fixture.subject, &fixture.id)
            .unwrap();
        let cwd = fixture._root.path().join("owned inert confirmation child");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(
            cwd.join("operation-fixture-enabled"),
            b"vesta-owned-inert-operation-fixture-v1",
        )
        .unwrap();
        let executable = cwd.join(if cfg!(windows) {
            "owned-confirmation-fixture.exe"
        } else {
            "owned-confirmation-fixture"
        });
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let launch = vesta_game_runtime::process::LaunchSpec::new(
            executable,
            vec![
                "--exact".into(),
                "operations::runner::tests::inert_game_fixture".into(),
                "--nocapture".into(),
            ],
            cwd.clone(),
            vesta_game_runtime::process::GracefulStop::MinecraftCommand,
        )
        .unwrap();
        // Install panic cleanup immediately, BEFORE admission/waits can fail.
        let owned = OwnedPublicRuntime {
            runtime: OperationRuntime::open(fixture.operations.clone(), launch).unwrap(),
            cwd,
        };
        let start = match fixture
            .operations
            .submit_game(&fixture.subject, GameCommand::Start, None)
            .unwrap()
        {
            super::super::Admission::Created(summary) => summary.id,
            _ => panic!("fresh native start admission"),
        };
        public_wait_for(|| {
            fixture
                .operations
                .get(&fixture.subject, &start)
                .unwrap()
                .unwrap()
                .state
                == OperationState::Succeeded
                && owned.cwd.join("operation-ready").exists()
                && owned.runtime.game_snapshot().unwrap().phase
                    == vesta_game_runtime::process::ProcessPhase::Running
        });
        let generation = owned
            .runtime
            .game_snapshot()
            .unwrap()
            .generation
            .expect("actual native generation");
        owned.runtime.request_shutdown();
        assert!(owned.runtime.status().unwrap().dispatch_stopped);
        let id = match fixture
            .operations
            .submit_game(
                &fixture.subject,
                GameCommand::ForceKill {
                    expected_generation: generation,
                },
                None,
            )
            .unwrap()
        {
            super::super::Admission::Created(summary) => summary.id,
            _ => panic!("fresh native destructive admission"),
        };
        Self {
            owned,
            fixture,
            id,
            generation,
        }
    }
    fn issue(&self, ttl: Duration) -> IssuedConfirmation {
        self.owned
            .runtime
            .issue_confirmation(&self.fixture.subject, &self.id, ttl)
            .unwrap()
    }
    fn state(&self) -> OperationState {
        self.fixture
            .operations
            .get(&self.fixture.subject, &self.id)
            .unwrap()
            .unwrap()
            .state
    }
    fn times(&self, hash: &str) -> (Option<i64>, Option<i64>) {
        self.fixture
            .write(|_, confirmations| confirmations.stored_times(hash))
            .unwrap()
            .unwrap()
    }
    fn journals(&self) -> (i64, i64) {
        self.fixture.db.read(|connection| Ok((
            connection.query_row("SELECT count(*) FROM audit_events WHERE operation_id=?1 AND action='confirmation.consume'",
                [self.id.as_str()], |row| row.get(0))?,
            connection.query_row("SELECT count(*) FROM events WHERE operation_id=?1",
                [self.id.as_str()], |row| row.get(0))?,
        ))).unwrap()
    }
    fn assert_no_native_effect(&self) {
        let started: bool = self
            .fixture
            .db
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT external_effect_started FROM operations WHERE id=?1",
                    [self.id.as_str()],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(!started);
        let snapshot = self.owned.runtime.game_snapshot().unwrap();
        assert_eq!(snapshot.generation, Some(self.generation));
        assert_eq!(
            snapshot.phase,
            vesta_game_runtime::process::ProcessPhase::Running
        );
    }
}
fn public_wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PUBLIC_WAIT;
    while !predicate() {
        assert!(
            std::time::Instant::now() < deadline,
            "actual confirmation native fixture timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn public_confirm_wrong_actor_or_device_commits_burn_and_denial_audit_and_replay_refuses() {
    for wrong_principal in [false, true] {
        let harness = PublicHarness::open();
        let issued = harness.issue(Duration::from_secs(60));
        let hash = issued.token.hash();
        let replay = ConfirmationToken::try_from(issued.token.expose_secret().to_owned()).unwrap();
        let before = harness.journals();
        let other_actor = if wrong_principal {
            Subject {
                principal: PrincipalId::parse("another-public-principal").unwrap(),
                device: harness.fixture.subject.device.clone(),
            }
        } else {
            Subject {
                principal: harness.fixture.subject.principal.clone(),
                device: DeviceId::parse("another-public-device").unwrap(),
            }
        };
        let error = harness
            .owned
            .runtime
            .confirm(&other_actor, &harness.id, issued.token)
            .unwrap_err();
        assert_eq!(error.code(), "confirmation_rejected");
        assert_eq!(harness.times(&hash), (Some(harness.fixture.now()), None));
        assert_eq!(harness.journals(), (before.0 + 1, before.1));
        assert_eq!(harness.state(), OperationState::AwaitingConfirmation);
        let error = harness
            .owned
            .runtime
            .confirm(&harness.fixture.subject, &harness.id, replay)
            .unwrap_err();
        assert_eq!(error.code(), "confirmation_rejected");
        assert_eq!(harness.journals(), (before.0 + 2, before.1));
        harness.assert_no_native_effect();
        assert!(
            harness
                .fixture
                .operations
                .audit
                .verify_chain()
                .unwrap()
                .valid
        );
    }
}

#[test]
fn public_refusal_audit_failure_rolls_back_burn_and_allows_audited_retry() {
    let harness = PublicHarness::open();
    let issued = harness.issue(Duration::from_secs(60));
    let hash = issued.token.hash();
    let retry = ConfirmationToken::try_from(issued.token.expose_secret().to_owned()).unwrap();
    let before = harness.journals();
    harness.fixture.audit_failure();
    let other_device = Subject {
        principal: harness.fixture.subject.principal.clone(),
        device: DeviceId::parse("another-public-device").unwrap(),
    };
    let error = harness
        .owned
        .runtime
        .confirm(&other_device, &harness.id, issued.token)
        .unwrap_err();
    assert_eq!(error.code(), "audit_unavailable");
    assert_eq!(error.primary().code(), "confirmation_rejected");
    assert_eq!(harness.times(&hash), (None, None));
    assert_eq!(harness.journals(), before);
    assert_eq!(harness.state(), OperationState::AwaitingConfirmation);
    harness
        .fixture
        .db
        .write(|connection| {
            connection.execute_batch("DROP TRIGGER confirmation_audit_failure")?;
            Ok(())
        })
        .unwrap();
    let after = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &harness.id, retry)
        .unwrap();
    assert_eq!(after.state, OperationState::Queued);
    assert_eq!(
        harness.times(&hash),
        (Some(harness.fixture.now()), Some(harness.fixture.now()))
    );
    assert_eq!(harness.journals(), (before.0 + 1, before.1 + 1));
    harness.assert_no_native_effect();
}

#[test]
fn public_acceptance_event_failure_rolls_back_receipt_queue_burn_and_success_audit() {
    let harness = PublicHarness::open();
    let issued = harness.issue(Duration::from_secs(60));
    let hash = issued.token.hash();
    let retry = ConfirmationToken::try_from(issued.token.expose_secret().to_owned()).unwrap();
    let before = harness.journals();
    harness
        .fixture
        .db
        .write(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER confirmation_event_failure BEFORE INSERT ON events \
            WHEN NEW.type='operation.state' BEGIN SELECT RAISE(ABORT,'fixture event failure'); END",
            )?;
            Ok(())
        })
        .unwrap();
    let error = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &harness.id, issued.token)
        .unwrap_err();
    assert!(matches!(error, OperationError::State(_)));
    assert_eq!(harness.times(&hash), (None, None));
    assert_eq!(harness.state(), OperationState::AwaitingConfirmation);
    assert_eq!(harness.journals(), before);
    harness.assert_no_native_effect();
    harness
        .fixture
        .db
        .write(|connection| {
            connection.execute_batch("DROP TRIGGER confirmation_event_failure")?;
            Ok(())
        })
        .unwrap();
    let after = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &harness.id, retry)
        .unwrap();
    assert_eq!(after.state, OperationState::Queued);
    assert_eq!(harness.journals(), (before.0 + 1, before.1 + 1));
    assert_eq!(
        harness.times(&hash),
        (Some(harness.fixture.now()), Some(harness.fixture.now()))
    );
    harness.assert_no_native_effect();
}

#[test]
fn public_foreign_operation_burns_presented_token_and_audits_without_queueing_either_operation() {
    let harness = PublicHarness::open();
    let issued = harness.issue(Duration::from_secs(60));
    let hash = issued.token.hash();
    let foreign_id = match harness
        .fixture
        .operations
        .submit_game(
            &harness.fixture.subject,
            GameCommand::ForceKill {
                expected_generation: harness.generation,
            },
            None,
        )
        .unwrap()
    {
        super::super::Admission::Created(summary) => summary.id,
        _ => panic!("fresh foreign destructive operation"),
    };
    let before = harness.journals();
    let foreign_events_before: i64 = harness
        .fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM events WHERE operation_id=?1",
                [foreign_id.as_str()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let error = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &foreign_id, issued.token)
        .unwrap_err();
    assert_eq!(error.code(), "confirmation_rejected");
    assert_eq!(harness.times(&hash), (Some(harness.fixture.now()), None));
    assert_eq!(harness.state(), OperationState::AwaitingConfirmation);
    assert_eq!(
        harness
            .fixture
            .operations
            .get(&harness.fixture.subject, &foreign_id)
            .unwrap()
            .unwrap()
            .state,
        OperationState::AwaitingConfirmation
    );
    assert_eq!(harness.journals(), before);
    let foreign_journals: (i64, i64) = harness.fixture.db.read(|connection| Ok((
        connection.query_row("SELECT count(*) FROM audit_events WHERE operation_id=?1 AND action='confirmation.consume' AND outcome='rejected'",
            [foreign_id.as_str()], |row| row.get(0))?,
        connection.query_row("SELECT count(*) FROM events WHERE operation_id=?1", [foreign_id.as_str()], |row| row.get(0))?,
    ))).unwrap();
    assert_eq!(foreign_journals, (1, foreign_events_before));
    harness.assert_no_native_effect();
}

#[test]
fn public_policy_revocation_between_issue_and_confirm_commits_only_burn_and_denial() {
    let harness = PublicHarness::open();
    let issued = harness.issue(Duration::from_secs(60));
    let hash = issued.token.hash();
    let before = harness.journals();
    harness.fixture.deny();
    let error = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &harness.id, issued.token)
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
    assert_eq!(harness.times(&hash), (Some(harness.fixture.now()), None));
    assert_eq!(harness.state(), OperationState::AwaitingConfirmation);
    assert_eq!(harness.journals(), (before.0 + 1, before.1));
    let error = harness
        .owned
        .runtime
        .issue_confirmation(
            &harness.fixture.subject,
            &harness.id,
            Duration::from_secs(60),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
    harness.assert_no_native_effect();
}

#[test]
fn public_expiry_boundary_burns_and_audits_without_receipt_or_queue() {
    let harness = PublicHarness::open();
    let issued = harness.issue(Duration::from_millis(10));
    assert_eq!(issued.expires_at_ms, harness.fixture.now() + 10);
    let hash = issued.token.hash();
    let before = harness.journals();
    harness
        .fixture
        .clock
        .0
        .store(issued.expires_at_ms, Ordering::SeqCst);
    let error = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &harness.id, issued.token)
        .unwrap_err();
    assert_eq!(error.code(), "confirmation_rejected");
    assert_eq!(harness.times(&hash), (Some(harness.fixture.now()), None));
    assert_eq!(harness.state(), OperationState::AwaitingConfirmation);
    assert_eq!(harness.journals(), (before.0 + 1, before.1));
    harness.assert_no_native_effect();
}

#[test]
fn public_acceptance_uses_real_generation_and_atomically_records_queue_receipt_and_event() {
    let harness = PublicHarness::open();
    let issued = harness.issue(Duration::from_secs(60));
    let hash = issued.token.hash();
    let before = harness.journals();
    let after = harness
        .owned
        .runtime
        .confirm(&harness.fixture.subject, &harness.id, issued.token)
        .unwrap();
    assert_eq!(after.state, OperationState::Queued);
    assert_eq!(harness.state(), OperationState::Queued);
    assert_eq!(
        harness.times(&hash),
        (Some(harness.fixture.now()), Some(harness.fixture.now()))
    );
    assert_eq!(harness.journals(), (before.0 + 1, before.1 + 1));
    harness
        .fixture
        .write(|repo, _| {
            let operation = repo.require(&harness.id)?;
            harness
                .fixture
                .operations
                .verify_binding(repo, &operation)?;
            verify_receipt_in(
                repo.connection(),
                harness.fixture.db.server_id(),
                &operation,
                &harness.owned.runtime.source_revision()?,
                harness.fixture.now(),
            )
        })
        .unwrap();
    harness.assert_no_native_effect();
    assert!(
        harness
            .fixture
            .operations
            .audit
            .verify_chain()
            .unwrap()
            .valid
    );
}
