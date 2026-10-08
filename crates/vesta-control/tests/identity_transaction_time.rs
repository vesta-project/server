//! Transaction-admission clock regressions over public identity APIs and real SQLite.
//!
//! The writer holder uses the database's actual immediate transaction/FIFO writer gate;
//! it is not a mocked service lock. Fixture identities are trusted SQL lookup records only.
use rusqlite::params;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;
use vesta_control::identity::{
    AuthorityChange, BuiltinRole, Capability, DenyReason, Effect, Expiry, IdentityService,
    PrincipalId, RemoteCommand, ResourceScope, RoleGrant, RoleId, Subject,
};
use vesta_state::audit::AuditLog;
use vesta_state::{Clock, StateDatabase, StateDirectory};

const WRITER_ENTER_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_START_TIMEOUT: Duration = Duration::from_secs(5);
const HELD_SAMPLE_OBSERVATION: Duration = Duration::from_millis(200);
const RELEASED_SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);
const HOLDER_FAILSAFE: Duration = Duration::from_secs(10);

/// Notifies exactly once after arming, allowing the tests to observe when production
/// code first samples time without replacing the real clock or transaction behavior.
struct ControlledClock {
    now_ms: AtomicI64,
    next_sample: Mutex<Option<Sender<()>>>,
}

impl ControlledClock {
    fn new(now_ms: i64) -> Self {
        Self {
            now_ms: AtomicI64::new(now_ms),
            next_sample: Mutex::new(None),
        }
    }

    fn set(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    fn watch_next_sample(&self) -> Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        *self
            .next_sample
            .lock()
            .expect("clock notification mutex poisoned") = Some(sender);
        receiver
    }
}

impl Clock for ControlledClock {
    fn millis(&self) -> i64 {
        if let Ok(mut notification) = self.next_sample.lock() {
            if let Some(sender) = notification.take() {
                let _ = sender.send(());
            }
        }
        self.now_ms.load(Ordering::SeqCst)
    }
}

struct Fixture {
    // Keep the owned local filesystem alive until after all database handles close.
    _root: TempDir,
    clock: Arc<ControlledClock>,
    db: StateDatabase,
    identities: IdentityService,
    audit: AuditLog,
    administrator: Subject,
    target: PrincipalId,
}

impl Fixture {
    fn open(now_ms: i64, administrator_expires_at: Option<i64>) -> Self {
        let root = tempfile::tempdir().expect("create identity transaction-time fixture");
        let state_path = root.path().join("private state");
        let game_path = root.path().join("game root");
        let directory = StateDirectory::new(&state_path, Some(&game_path))
            .expect("create separate state and game paths");
        let clock = Arc::new(ControlledClock::new(now_ms));
        let db = StateDatabase::open(directory, clock.clone()).expect("open real SQLite state");
        let identities = IdentityService::new(db.clone()).expect("create public identity service");
        let audit = AuditLog::new(db.clone()).expect("open real audit chain");

        let administrator = Subject {
            principal: PrincipalId::parse("10000000-0000-4000-8000-000000000001")
                .expect("canonical fixture administrator id"),
            device: vesta_control::identity::DeviceId::parse(
                "10000000-0000-4000-8000-000000000002",
            )
            .expect("canonical fixture administrator device id"),
        };
        let target = PrincipalId::parse("10000000-0000-4000-8000-000000000003")
            .expect("canonical fixture target id");
        let target_device = "10000000-0000-4000-8000-000000000004";
        let server_id = db.server_id().to_owned();

        // This deliberately bypasses enrollment only to create trusted test records.
        // Subject remains just the two lookup IDs; the production API reloads all authority.
        db.write(|connection| {
            connection.execute(
                "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) \
                 VALUES (?1, ?2, 'Fixture administrator', 'active', ?3, ?3)",
                params![administrator.principal.as_str(), server_id, now_ms],
            )?;
            connection.execute(
                "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at) \
                 VALUES (?1, ?2, ?3, 'Fixture administrator device', ?4, ?5)",
                params![
                    administrator.device.as_str(),
                    server_id,
                    administrator.principal.as_str(),
                    "a".repeat(64),
                    now_ms
                ],
            )?;
            connection.execute(
                "INSERT INTO role_assignments (id, principal_id, role_id, granted_by, created_at, expires_at) \
                 VALUES (?1, ?2, 'administrator', 'trusted-fixture', ?3, ?4)",
                params![
                    "10000000-0000-4000-8000-000000000005",
                    administrator.principal.as_str(),
                    now_ms,
                    administrator_expires_at
                ],
            )?;
            connection.execute(
                "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) \
                 VALUES (?1, ?2, 'Fixture target', 'active', ?3, ?3)",
                params![target.as_str(), server_id, now_ms],
            )?;
            connection.execute(
                "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at) \
                 VALUES (?1, ?2, ?3, 'Fixture target device', ?4, ?5)",
                params![
                    target_device,
                    server_id,
                    target.as_str(),
                    "b".repeat(64),
                    now_ms
                ],
            )?;
            Ok(())
        })
        .expect("seed trusted administrator and active target rows");

        Self {
            _root: root,
            clock,
            db,
            identities,
            audit,
            administrator,
            target,
        }
    }

    /// Holds an actual `BEGIN IMMEDIATE` transaction until the caller releases it.
    fn hold_writer(&self) -> HeldWriter {
        HeldWriter::start(self.db.clone())
    }

    fn active_target_grant_count(&self, role_id: &str) -> i64 {
        self.db
            .read(|connection| {
                connection
                    .query_row(
                        "SELECT count(*) FROM role_assignments \
                     WHERE principal_id = ?1 AND role_id = ?2 AND revoked_at IS NULL",
                        params![self.target.as_str(), role_id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("read target role assignment count")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.db.close();
    }
}

/// Releases and joins the holder on ordinary completion and on unwinding. The fallback
/// timeout inside the callback is a second guard against a permanently held test writer.
struct HeldWriter {
    release: Option<Sender<()>>,
    entered: Receiver<()>,
    thread: Option<JoinHandle<std::result::Result<(), String>>>,
}

impl HeldWriter {
    fn start(db: StateDatabase) -> Self {
        let (entered_sender, entered) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            db.write(|_| {
                let _ = entered_sender.send(());
                release_receiver
                    .recv_timeout(HOLDER_FAILSAFE)
                    .map_err(|_| vesta_state::StateError::new("state_test_release_timeout"))?;
                Ok(())
            })
            .map_err(|error| error.code().to_owned())
        });
        Self {
            release: Some(release),
            entered,
            thread: Some(thread),
        }
    }

    fn wait_until_held(&self) -> bool {
        self.entered.recv_timeout(WRITER_ENTER_TIMEOUT).is_ok()
    }

    fn release_sender(&self) -> Sender<()> {
        self.release
            .as_ref()
            .expect("writer release sender is present until release")
            .clone()
    }

    fn release(&mut self) {
        if let Some(sender) = self.release.take() {
            let _ = sender.send(());
        }
    }

    fn join(&mut self) -> Option<thread::Result<std::result::Result<(), String>>> {
        self.thread.take().map(JoinHandle::join)
    }
}

impl Drop for HeldWriter {
    fn drop(&mut self) {
        self.release();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A request thread also releases the holder before joining if the test unwinds early.
struct PendingRequest<T> {
    release: Sender<()>,
    started: Receiver<()>,
    thread: Option<JoinHandle<T>>,
}

impl<T> PendingRequest<T> {
    fn start(release: Sender<()>, action: impl FnOnce() -> T + Send + 'static) -> Self
    where
        T: Send + 'static,
    {
        let (started_sender, started) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _ = started_sender.send(());
            action()
        });
        Self {
            release,
            started,
            thread: Some(thread),
        }
    }

    fn wait_until_started(&self) -> bool {
        self.started.recv_timeout(REQUEST_START_TIMEOUT).is_ok()
    }

    fn join(&mut self) -> Option<thread::Result<T>> {
        self.thread.take().map(JoinHandle::join)
    }
}

impl<T> Drop for PendingRequest<T> {
    fn drop(&mut self) {
        let _ = self.release.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct HeldRun<T> {
    holder_entered: bool,
    request_started: bool,
    no_clock_sample_while_held: bool,
    clock_sample_after_release: bool,
    holder_result: Option<thread::Result<std::result::Result<(), String>>>,
    request_result: Option<thread::Result<T>>,
}

/// Run an operation that queues behind a real SQLite writer, advance time while it is
/// queued, then release and join both workers before returning anything the test asserts.
fn run_behind_writer<T: Send + 'static>(
    fixture: &Fixture,
    admitted_time_ms: i64,
    action: impl FnOnce() -> T + Send + 'static,
) -> HeldRun<T> {
    let mut holder = fixture.hold_writer();
    let holder_entered = holder.wait_until_held();
    let samples = fixture.clock.watch_next_sample();
    let mut request =
        holder_entered.then(|| PendingRequest::start(holder.release_sender(), action));
    let request_started = request
        .as_ref()
        .is_some_and(PendingRequest::wait_until_started);

    // This bounded window detects an eager pre-admission clock read; it cannot prove
    // scheduler fairness. The post-release check and the persisted outcome provide the
    // complementary evidence that the request actually proceeds after writer admission.
    let no_clock_sample_while_held = matches!(
        samples.recv_timeout(HELD_SAMPLE_OBSERVATION),
        Err(RecvTimeoutError::Timeout)
    );
    fixture.clock.set(admitted_time_ms);
    holder.release();
    let holder_result = holder.join();
    let request_result = request.as_mut().and_then(PendingRequest::join);
    let clock_sample_after_release = samples.recv_timeout(RELEASED_SAMPLE_TIMEOUT).is_ok();

    HeldRun {
        holder_entered,
        request_started,
        no_clock_sample_while_held,
        clock_sample_after_release,
        holder_result,
        request_result,
    }
}

#[test]
fn expired_administrator_waiting_for_writer_is_denied_after_admission_and_audited() {
    let fixture = Fixture::open(1_000, Some(1_001));
    let identities = fixture.identities.clone();
    let actor = fixture.administrator.clone();
    let target = fixture.target.clone();

    let run = run_behind_writer(&fixture, 1_002, move || {
        identities.apply_remote(
            &actor,
            RemoteCommand::AssignRole {
                principal: target,
                role: RoleId::from(BuiltinRole::PackUser),
                expiry: Expiry::Never,
            },
        )
    });

    // All worker handles have been joined by the helper before any assertion can panic.
    assert!(
        run.holder_entered,
        "the holder must own a SQLite writer transaction"
    );
    assert!(
        run.request_started,
        "the actor request thread must start before observation"
    );
    assert!(
        run.no_clock_sample_while_held,
        "time must not be sampled while queued for the writer"
    );
    assert!(
        run.clock_sample_after_release,
        "time must be sampled after writer admission"
    );
    assert!(matches!(run.holder_result, Some(Ok(Ok(())))));
    match run.request_result {
        Some(Ok(Err(error))) => {
            assert_eq!(error.code(), "forbidden");
            assert_eq!(error.reason(), Some(DenyReason::NotGranted));
        }
        other => panic!("expired administrator request must be denied, got {other:?}"),
    }
    assert_eq!(fixture.active_target_grant_count("pack-user"), 0);

    let verification = fixture
        .audit
        .verify_chain()
        .expect("verify denial audit chain");
    assert!(verification.valid, "denial audit chain must verify");
    let entries = fixture.audit.page(0, 500).expect("read denial audit");
    let denial = entries
        .iter()
        .find(|entry| {
            entry.action == "authority.role.assign"
                && entry.target == format!("principal:{}", fixture.target.as_str())
                && entry.outcome == "denied"
        })
        .expect("expired authority refusal must have a denial audit entry");
    assert_eq!(
        denial.principal_id,
        fixture.administrator.principal.as_str()
    );
    assert_eq!(denial.device_id, fixture.administrator.device.as_str());
    assert_eq!(denial.source, "remote");
    assert_eq!(denial.at_ms, 1_002);
    assert_eq!(denial.metadata_json, r#"{"reason":"not_granted"}"#);
}

#[test]
fn relative_grant_expiry_starts_at_the_time_the_writer_transaction_is_admitted() {
    let fixture = Fixture::open(1_000, None);
    let identities = fixture.identities.clone();
    let actor = fixture.administrator.clone();
    let target = fixture.target.clone();

    let run = run_behind_writer(&fixture, 2_000, move || {
        identities.apply_remote(
            &actor,
            RemoteCommand::SetOverride {
                principal: target,
                grant: RoleGrant::new(Capability::PlayersView, ResourceScope::ALL)
                    .expect("valid fixture grant"),
                effect: Effect::Allow,
                expiry: Expiry::After(Duration::from_millis(10)),
            },
        )
    });

    // Both real database workers are joined before inspecting results or stored rows.
    assert!(
        run.holder_entered,
        "the holder must own a SQLite writer transaction"
    );
    assert!(
        run.request_started,
        "the grant request thread must start before observation"
    );
    assert!(
        run.no_clock_sample_while_held,
        "relative expiry must not start before admission"
    );
    assert!(
        run.clock_sample_after_release,
        "the admitted request must sample its clock"
    );
    assert!(matches!(run.holder_result, Some(Ok(Ok(())))));
    assert!(matches!(
        run.request_result,
        Some(Ok(Ok(AuthorityChange::Override(_))))
    ));

    let stored_expiry = fixture
        .db
        .read(|connection| {
            connection
                .query_row(
                    "SELECT expires_at FROM principal_overrides \
                 WHERE principal_id = ?1 AND capability = 'players.view' AND effect = 'allow'",
                    params![fixture.target.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(Into::into)
        })
        .expect("read admitted relative grant expiry");
    assert_eq!(
        stored_expiry, 2_010,
        "expiry is admission time 2000 + 10ms, not request time 1000 + 10ms"
    );
    assert!(
        fixture
            .audit
            .verify_chain()
            .expect("verify grant audit chain")
            .valid,
        "successful grant audit chain must verify"
    );
}
