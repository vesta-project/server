//! Bounded native lifecycle checks over real protected SQLite and owned inert
//! copies of this test executable. No services, Minecraft/Java, provider or shell.
use super::*;
use crate::identity::{Capability, DeviceId, PrincipalId, Subject};
use rusqlite::params;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, Write},
    path::PathBuf,
    sync::mpsc,
};
use tempfile::TempDir;
use vesta_game_runtime::process::{GracefulStop, ProcessPhase};
use vesta_state::{Clock, StateDatabase, StateDirectory};

const WAIT: Duration = Duration::from_secs(15);
const ENABLE: &[u8] = b"vesta-owned-inert-operation-fixture-v1";
#[path = "world_tests.rs"]
mod world_tests;

#[test]
fn inert_game_fixture() {
    if fs::read("operation-fixture-enabled").ok().as_deref() != Some(ENABLE) {
        return;
    }
    let ignored = fs::read("ignore-stop").ok().as_deref() == Some(b"ignore".as_slice());
    let mut commands = OpenOptions::new()
        .create(true)
        .append(true)
        .open("operation-commands")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    // This subprocess is only an inert fixture. Its fixed marker provides panic
    // cleanup without teaching production runtime Drop to signal or kill games.
    let input = thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let sleep_address = fs::read_to_string("sleep-status-address")
        .ok()
        .map(|v| v.parse::<std::net::SocketAddr>().unwrap());
    let _sleep_status=sleep_address.map(|address|vesta_game_runtime::offline::Responder::bind(vesta_game_runtime::offline::Configuration {
        address, status_json:r#"{"version":{"name":"Owned fixture","protocol":767},"players":{"online":0,"max":20},"description":{"text":"Owned fixture"}}"#.into(),disconnect:"Owned fixture".into(),wake:false,
    }).unwrap());
    println!("inert-operation-fixture-ready");
    if std::path::Path::new("metrics-tick-fixture").is_file() || sleep_address.is_some() {
        println!("[12:00:00] [Server thread/INFO]: Done (1.234s)! For help, type \"help\"");
    }
    std::io::stdout().flush().unwrap();
    fs::write("operation-ready", b"ready").unwrap();
    loop {
        if fs::read("release-game").ok().as_deref() == Some(b"release".as_slice()) {
            // Fixed owned fixture escape, never a management/process API.
            std::process::exit(0);
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(line) => {
                writeln!(commands, "{line}").unwrap();
                commands.flush().unwrap();
                if line == "vesta-fixture-ready"
                    && std::path::Path::new("whitelist-fixture").is_file()
                {
                    println!(
                        "[12:00:01] [Server thread/INFO]: Done (1.234s)! For help, type \"help\""
                    );
                    std::io::stdout().flush().unwrap();
                }
                if sleep_address.is_some() {
                    if line == "list" && !std::path::Path::new("sleep-no-list").is_file() {
                        println!(
                            "[12:00:01] [Server thread/INFO]: There are 0 of a max of 20 players online:"
                        );
                    }
                    if let Some(warning) = line.strip_prefix("say ") {
                        println!("[12:00:02] [Server thread/INFO]: [Server] {warning}");
                    }
                    if line == "list" && std::path::Path::new("sleep-join-after-list").is_file() {
                        println!("[12:00:03] [Server thread/INFO]: Alex joined the game");
                    }
                    std::io::stdout().flush().unwrap();
                }
                if std::path::Path::new("metrics-tick-fixture").is_file() && line == "tps" {
                    println!("[12:00:01 INFO]: <Alex> TPS from last 1m, 5m, 15m: 1.0, 1.0, 1.0");
                    if !std::path::Path::new("metrics-tick-no-response").is_file() {
                        println!("[12:00:01 INFO]: TPS from last 1m, 5m, 15m: 19.5, 19.8, 20.0");
                    }
                    std::io::stdout().flush().unwrap();
                }
                if line == "stop" && !ignored {
                    std::process::exit(0);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                input.join().unwrap();
                return;
            }
        }
    }
}

struct TestClock;
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}
struct Harness {
    _root: TempDir,
    cwd: PathBuf,
    db: StateDatabase,
    operations: Operations,
    subject: Subject,
}
impl Harness {
    fn new(ignore_stop: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("inert game");
        fs::create_dir(&cwd).unwrap();
        fs::write(cwd.join("operation-fixture-enabled"), ENABLE).unwrap();
        if ignore_stop {
            fs::write(cwd.join("ignore-stop"), b"ignore").unwrap();
        }
        let directory = StateDirectory::new(root.path().join("private state"), None).unwrap();
        let db = StateDatabase::open(directory, Arc::new(TestClock)).unwrap();
        let subject = Subject {
            principal: PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
            device: DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        };
        db.write(|connection| {
            connection.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Owned inert fixture','active',?,?)",
                params![subject.principal.as_str(),db.server_id(),db.clock().millis(),db.clock().millis()])?;
            connection.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Owned inert fixture',?,?)",
                params![subject.device.as_str(),db.server_id(),subject.principal.as_str(),"a".repeat(64),db.clock().millis()])?;
            connection.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(),subject.principal.as_str(),db.clock().millis()])?;
            Ok(())
        }).unwrap();
        let operations = Operations::new(db.clone()).unwrap();
        Self {
            _root: root,
            cwd,
            db,
            operations,
            subject,
        }
    }
    fn launch(&self) -> LaunchSpec {
        let executable = self.cwd.join(if cfg!(windows) {
            "owned-operation-fixture.exe"
        } else {
            "owned-operation-fixture"
        });
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        LaunchSpec::new(
            executable,
            vec![
                "--exact".into(),
                "operations::runner::tests::inert_game_fixture".into(),
                "--nocapture".into(),
            ],
            self.cwd.clone(),
            GracefulStop::MinecraftCommand,
        )
        .unwrap()
    }
    fn runtime(&self, stop_wait: Duration) -> OwnedRuntime {
        OwnedRuntime {
            runtime: OperationRuntime::open_with_stop_timeout(
                self.operations.clone(),
                self.launch(),
                stop_wait,
            )
            .unwrap(),
            cwd: self.cwd.clone(),
            gate_release: None,
        }
    }
    fn submit(&self, command: GameCommand) -> OperationId {
        match self
            .operations
            .submit_game(&self.subject, command, None)
            .unwrap()
        {
            super::super::Admission::Created(summary) => summary.id,
            super::super::Admission::Existing(_) => panic!("fresh fixture has no idempotency key"),
        }
    }
    fn state(&self, id: &OperationId) -> super::super::OperationState {
        self.operations
            .get(&self.subject, id)
            .unwrap()
            .unwrap()
            .state
    }
    fn wait_state(&self, id: &OperationId, state: super::super::OperationState) {
        wait_for(|| self.state(id) == state);
    }
    fn facts(&self, id: &OperationId) -> (bool, Option<String>, i64, Option<String>) {
        self.db.read(|connection| {
            let (marker, token, error) = connection.query_row(
                "SELECT external_effect_started,owner_token,error_code FROM operations WHERE id=?", [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            let locks = connection.query_row("SELECT count(*) FROM resource_locks WHERE operation_id=?",
                [id.as_str()], |row| row.get(0))?;
            Ok((marker, token, locks, error))
        }).unwrap()
    }
    fn deny_stop(&self) {
        self.db.write(|connection| {
            connection.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'*','deny','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(),self.subject.principal.as_str(),Capability::LifecycleStop.id(),self.db.clock().millis()])?;
            Ok(())
        }).unwrap();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::write(self.cwd.join("release-game"), b"release");
        let result = self.db.close();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
struct OwnedRuntime {
    runtime: OperationRuntime,
    cwd: PathBuf,
    gate_release: Option<mpsc::Sender<()>>,
}
impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        // Test-fixture-only signals precede runtime's blocking observation Drop.
        // Both a pending callback and an ignoring child must actually return.
        if let Some(release) = &self.gate_release {
            let _ = release.send(());
        }
        let _ = fs::write(self.cwd.join("release-game"), b"release");
    }
}

// A raw Driver has observation-only Drop. This fixture wrapper accompanies it
// from construction through every assertion/wait and the spawn closure itself,
// so unwind or spawn failure releases native fixture work BEFORE Driver drops.
struct FixtureDriver {
    driver: Driver,
    cwd: PathBuf,
    gate_release: mpsc::Sender<()>,
}
impl FixtureDriver {
    fn run(mut self) -> OperationsResult<()> {
        self.driver.run()
    }
}
impl std::ops::Deref for FixtureDriver {
    type Target = Driver;
    fn deref(&self) -> &Driver {
        &self.driver
    }
}
impl std::ops::DerefMut for FixtureDriver {
    fn deref_mut(&mut self) -> &mut Driver {
        &mut self.driver
    }
}
impl Drop for FixtureDriver {
    fn drop(&mut self) {
        let _ = self.gate_release.send(());
        let _ = fs::write(self.cwd.join("release-game"), b"release");
        // The owned Driver field drops only after these fixture escape signals.
    }
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < until, "owned operation fixture timed out");
        thread::sleep(DRIVER_INTERVAL);
    }
}
fn start(harness: &Harness, runtime: &OperationRuntime) -> GenerationId {
    let id = harness.submit(GameCommand::Start);
    harness.wait_state(&id, super::super::OperationState::Succeeded);
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let generation = runtime.game_snapshot().unwrap().generation.unwrap();
    assert!(harness.facts(&id).0);
    generation
}

#[test]
fn idle_runtime_autonomously_owns_one_os_lock_and_database_lease_without_starting_game() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime(Duration::from_millis(25));
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    assert!(!harness.cwd.join("operation-ready").exists());
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    assert!(OperationRuntime::open(harness.operations.clone(), harness.launch()).is_err());
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert!(harness.db.directory().acquire_runner_lock().is_ok());
}

#[test]
fn autonomous_start_and_cooperative_guarded_stop_complete_real_exit_and_all_io_joins() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime(Duration::from_secs(1));
    let generation = start(&harness, &owned.runtime);
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    harness.wait_state(&stop, super::super::OperationState::Succeeded);
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "stop\n"
    );
    assert_eq!(harness.facts(&stop), (true, None, 0, None));
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert!(harness.operations.audit.verify_chain().unwrap().valid);
}

#[test]
fn queue_wake_is_retained_when_admission_precedes_idle_wait() {
    let wake = WorkWake::default();
    let before_admission = wake.version();
    wake.notify();
    let started = Instant::now();
    assert!(wake.wait_for_change(before_admission, Duration::from_secs(5)));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn idle_runtime_dispatches_preexisting_and_directly_admitted_work() {
    let harness = Harness::new(false);
    // A durable operation can predate this runtime (for example, agent restart).
    let start_id = harness.submit(GameCommand::Start);
    let mut owned = harness.runtime(Duration::from_secs(1));
    harness.wait_state(&start_id, super::super::OperationState::Succeeded);
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let generation = owned.runtime.game_snapshot().unwrap().generation.unwrap();

    // Lower-level admissions do not send a RuntimeHandle notification. The
    // bounded read-only fallback must discover them even while the game is live.
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    harness.wait_state(&stop, super::super::OperationState::Succeeded);
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

#[test]
fn idle_runtime_wakes_for_network_admission_without_waiting_for_fallback() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime(Duration::from_secs(1));
    // Let the initial empty scan finish, then observe notification-driven pickup.
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let accepted = owned
        .runtime
        .network_handle()
        .submit_game(&harness.subject, GameCommand::Start, "wake-idle-runtime")
        .unwrap();
    let super::super::Admission::Created(operation) = accepted else {
        panic!("fresh intent")
    };
    wait_for(|| harness.state(&operation.id) != super::super::OperationState::Queued);
    assert!(
        started.elapsed() < IDLE_QUEUE_PROBE,
        "network admission waited for idle fallback"
    );
    harness.wait_state(&operation.id, super::super::OperationState::Succeeded);
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let generation = owned.runtime.game_snapshot().unwrap().generation.unwrap();
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    harness.wait_state(&stop, super::super::OperationState::Succeeded);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

#[test]
fn acknowledged_uncooperative_stop_fails_boundedly_but_retains_generation_for_confirmed_force_kill()
{
    let harness = Harness::new(true);
    let mut owned = harness.runtime(Duration::from_millis(25));
    let generation = start(&harness, &owned.runtime);
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    harness.wait_state(&stop, super::super::OperationState::Failed);
    assert_eq!(
        harness.facts(&stop),
        (true, None, 0, Some("game_stop_timeout".into()))
    );
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "stop\n"
    );
    let snapshot = owned.runtime.game_snapshot().unwrap();
    assert_eq!(snapshot.generation, Some(generation));
    assert_eq!(snapshot.phase, ProcessPhase::Stopping);
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());

    let kill = harness.submit(GameCommand::ForceKill {
        expected_generation: generation,
    });
    assert_eq!(
        harness.state(&kill),
        super::super::OperationState::AwaitingConfirmation
    );
    assert!(!harness.facts(&kill).0);
    let confirmation = owned
        .runtime
        .issue_confirmation(&harness.subject, &kill, Duration::from_secs(60))
        .unwrap();
    owned
        .runtime
        .confirm(&harness.subject, &kill, confirmation.token)
        .unwrap();
    harness.wait_state(&kill, super::super::OperationState::Succeeded);
    assert_eq!(owned.runtime.game_snapshot().unwrap().generation, None);
    assert!(harness.facts(&kill).0);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert!(harness.operations.audit.verify_chain().unwrap().valid);
}

#[test]
fn exited_child_with_pending_owned_writer_is_genuinely_draining_until_actual_ack_and_join() {
    let harness = Harness::new(false);
    let (mut owned, stop, release) = delayed_stop_ack(&harness, false);
    let generation = owned.runtime.game_snapshot().unwrap().generation.unwrap();
    // The cooperating native child actually exits after the fixed stop write.
    // Its owned writer is still in callback destruction, before typed ACK.
    wait_for(|| owned.runtime.game_snapshot().unwrap().phase == ProcessPhase::Draining);
    thread::sleep(Duration::from_millis(60));
    let facts = harness.facts(&stop);
    assert!(facts.0);
    assert!(facts.1.is_some());
    assert_eq!(facts.2, 1);
    assert_eq!(harness.state(&stop), super::super::OperationState::Running);
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().generation,
        Some(generation)
    );
    assert_eq!(
        owned.runtime.source_revision().unwrap(),
        SourceRevision::for_game(None)
    );
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    release.send(()).unwrap();
    harness.wait_state(&stop, super::super::OperationState::Succeeded);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert_eq!(harness.facts(&stop), (true, None, 0, None));
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
}

#[test]
fn returned_stop_exit_deadline_applies_only_to_reliable_live_phases_not_draining_or_uncertainty() {
    let deadline = Instant::now();
    let before = deadline.checked_sub(Duration::from_nanos(1)).unwrap();
    for phase in [ProcessPhase::Running, ProcessPhase::Stopping] {
        assert!(!stop_deadline_elapsed(phase, None, Some(deadline), before));
        assert!(stop_deadline_elapsed(phase, None, Some(deadline), deadline));
        assert!(!stop_deadline_elapsed(
            phase,
            Some("game_observe_failed"),
            Some(deadline),
            deadline
        ));
        assert!(!stop_deadline_elapsed(phase, None, None, deadline));
    }
    for phase in [ProcessPhase::Stopped, ProcessPhase::Draining] {
        assert!(!stop_deadline_elapsed(
            phase,
            None,
            Some(deadline),
            deadline
        ));
        assert!(!stop_deadline_elapsed(
            phase,
            Some("game_observe_failed"),
            Some(deadline),
            deadline
        ));
    }
}

#[test]
fn returned_stop_ack_with_second_owned_writer_draining_retains_lock_until_actual_join() {
    let harness = Harness::new(false);
    let mut owner = harness.operations.open_owner().unwrap();
    let mut game = GameLifecycle::new(harness.launch());
    let generation = game.start(|_| Ok(())).unwrap();
    game.publish().unwrap();
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    let claim = owner
        .claim_next_with_revision(&game.revision())
        .unwrap()
        .unwrap();
    owner.begin_run(&claim, &game.revision()).unwrap();
    let native = owner.native_effect_guard(&claim).unwrap();
    let retained_core = owner.native_effect_guard(&claim).unwrap();
    let view = game.view.clone();
    let (first_entered_tx, first_entered_rx) = mpsc::channel();
    let (first_release_tx, first_release_rx) = mpsc::channel();
    let mut first_gate = Some((first_entered_tx, first_release_rx));
    let receipt = game
        .process
        .queue_stop_guarded(generation, move || {
            let (entered, release) = first_gate.take().unwrap();
            entered.send(()).unwrap();
            release
                .recv_timeout(WAIT)
                .map_err(|_| StateError::new("state_owned_test_gate_closed"))?;
            native.begin_effect(&view.revision()?)
        })
        .unwrap();
    first_entered_rx.recv_timeout(WAIT).unwrap();
    let (second_entered_tx, second_entered_rx) = mpsc::channel();
    let (second_release_tx, second_release_rx) = mpsc::channel();
    let mut second_gate = Some((second_entered_tx, second_release_rx));
    let mut second_receipt = game
        .process
        .queue_stop_guarded(generation, move || {
            // This private witness retains the same exact core but NEVER authorizes
            // or writes a second stop. Its typed fixed refusal follows release.
            let _owned_core = &retained_core;
            let (entered, release) = second_gate.take().unwrap();
            entered.send(()).unwrap();
            let _ = release.recv_timeout(WAIT);
            Err(OperationError::rejected(OperationErrorCode::InvalidRequest))
        })
        .unwrap();
    first_release_tx.send(()).unwrap();
    // FIFO writer admission proves the first request/write/ACK actually returned
    // before the second callback entered; no synthetic marker/ACK is introduced.
    second_entered_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(second_receipt.try_result(), None);
    let operations = owner.operations().clone();
    let (shared, players, ticks) = DriverState::new(&owner, &game);
    let mut driver = FixtureDriver {
        driver: Driver {
            local_receiver: mpsc::sync_channel(1).1,
            owner,
            game,
            players,
            ticks,
            active: Some(Active::StopWrite {
                claim,
                generation,
                receipt,
                restart: false,
            }),
            shared: shared.clone(),
            stop_wait: Duration::from_millis(25),
            observation_uncertain: None,
            after_dispatch: None,
        },
        cwd: harness.cwd.clone(),
        gate_release: second_release_tx.clone(),
    };
    // Observe actual native exit before starting the short deadline, eliminating
    // scheduler timing as a substitute for a real Draining witness.
    wait_for(|| {
        driver.game.observe().unwrap();
        driver.game.publish().unwrap();
        driver.game.process.snapshot().phase == ProcessPhase::Draining
    });
    driver.advance().unwrap();
    assert!(matches!(
        &driver.active,
        Some(Active::Exit {
            uncertain: None,
            stop_deadline: Some(_),
            ..
        })
    ));
    let handle = thread::spawn(move || driver.run());
    let mut owned = OwnedRuntime {
        runtime: OperationRuntime {
            operations,
            start_block: Default::default(),
            shared: shared.clone(),
            local: LocalRuntime {
                wake: shared.wake.clone(),
                shared: shared.clone(),
                sender: mpsc::sync_channel(1).0,
            },
            driver: Some(handle),
        },
        cwd: harness.cwd.clone(),
        gate_release: Some(second_release_tx.clone()),
    };
    wait_for(|| owned.runtime.game_snapshot().unwrap().phase == ProcessPhase::Draining);
    thread::sleep(Duration::from_millis(60));
    let facts = harness.facts(&stop);
    assert!(facts.0);
    assert!(facts.1.is_some());
    assert_eq!(facts.2, 1);
    assert_eq!(harness.state(&stop), super::super::OperationState::Running);
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().generation,
        Some(generation)
    );
    assert_eq!(second_receipt.try_result(), None);
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    second_release_tx.send(()).unwrap();
    harness.wait_state(&stop, super::super::OperationState::Succeeded);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert_eq!(
        second_receipt.try_result(),
        Some(Err(GuardedError::Guard(OperationError::rejected(
            OperationErrorCode::InvalidRequest
        ))))
    );
    assert_eq!(harness.facts(&stop), (true, None, 0, None));
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "stop\n"
    );
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
}

#[test]
fn os_local_lifecycle_uses_owned_inert_child_without_minting_policy() {
    let harness = Harness::new(false);
    // Local OS-account decisions must not bootstrap canonical remote authority.
    harness
        .db
        .write(|c| {
            c.execute_batch(
                "DELETE FROM role_assignments; DELETE FROM devices; DELETE FROM principals;",
            )?;
            Ok(())
        })
        .unwrap();
    let mut owned = harness.runtime(Duration::from_secs(5));
    let local = owned.runtime.local_handle();
    assert_eq!(
        local.request(LocalAction::Stop),
        Err("local_game_not_running")
    );
    local.request(LocalAction::Start).unwrap();
    let first = owned.runtime.game_snapshot().unwrap().generation.unwrap();
    wait_for(|| harness.cwd.join("operation-ready").exists());
    assert_eq!(
        local.request(LocalAction::Start),
        Err("local_start_refused")
    );
    local.request(LocalAction::Restart).unwrap();
    let second = owned.runtime.game_snapshot().unwrap().generation.unwrap();
    assert_ne!(first, second);
    local.request(LocalAction::Stop).unwrap();
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    assert_eq!(
        fs::read(harness.cwd.join("operation-commands")).unwrap(),
        b"stop\nstop\n"
    );
    harness
        .db
        .read(|c| {
            for table in ["principals", "devices", "role_assignments"] {
                let count: i64 =
                    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
                assert_eq!(count, 0);
            }
            Ok(())
        })
        .unwrap();
    assert!(harness.operations.audit.verify_chain().unwrap().valid);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert_eq!(
        local.request(LocalAction::Start),
        Err("local_runtime_unavailable")
    );
}

#[test]
fn os_local_restart_timeout_retains_child_and_never_force_kills_or_restarts() {
    let harness = Harness::new(true);
    let mut owned = harness.runtime(Duration::from_millis(100));
    let local = owned.runtime.local_handle();
    local.request(LocalAction::Start).unwrap();
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let generation = owned.runtime.game_snapshot().unwrap().generation;
    assert_eq!(
        local.request(LocalAction::Restart),
        Err("local_stop_timed_out_no_force_kill")
    );
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().generation,
        generation
    );
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopping
    );
    assert_eq!(
        fs::read(harness.cwd.join("operation-commands")).unwrap(),
        b"stop\n"
    );
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

/// Exercise the actual kernel/native writer seam with a private owned gate. No
/// exported test feature, authority/provider registry or operation fixture binding.
fn delayed_stop(
    harness: &Harness,
) -> (
    OwnedRuntime,
    OperationId,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
) {
    let mut owner = harness.operations.open_owner().unwrap();
    let mut game = GameLifecycle::new(harness.launch());
    let generation = game.start(|_| Ok(())).unwrap();
    game.publish().unwrap();
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let id = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    let claim = owner
        .claim_next_with_revision(&game.revision())
        .unwrap()
        .unwrap();
    assert_eq!(claim.id(), &id);
    owner.begin_run(&claim, &game.revision()).unwrap();
    let native = owner.native_effect_guard(&claim).unwrap();
    let view = game.view.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut gate = Some((entered_tx, release_rx));
    let receipt = game
        .process
        .queue_stop_guarded(generation, move || {
            let (entered, release) = gate.take().expect("native callback exactly once");
            entered.send(()).unwrap();
            release
                .recv_timeout(WAIT)
                .map_err(|_| StateError::new("state_owned_test_gate_closed"))?;
            native.begin_effect(&view.revision()?)
        })
        .unwrap();
    let operations = owner.operations().clone();
    let (shared, players, ticks) = DriverState::new(&owner, &game);
    let driver = FixtureDriver {
        driver: Driver {
            local_receiver: mpsc::sync_channel(1).1,
            owner,
            game,
            players,
            ticks,
            active: Some(Active::StopWrite {
                claim,
                generation,
                receipt,
                restart: false,
            }),
            shared: shared.clone(),
            stop_wait: Duration::from_millis(25),
            observation_uncertain: None,
            after_dispatch: None,
        },
        cwd: harness.cwd.clone(),
        gate_release: release_tx.clone(),
    };
    let handle = thread::spawn(move || driver.run());
    let runtime = OperationRuntime {
        operations,
        start_block: Default::default(),
        shared: shared.clone(),
        local: LocalRuntime {
            wake: shared.wake.clone(),
            shared: shared.clone(),
            sender: mpsc::sync_channel(1).0,
        },
        driver: Some(handle),
    };
    (
        OwnedRuntime {
            runtime,
            cwd: harness.cwd.clone(),
            gate_release: Some(release_tx.clone()),
        },
        id,
        release_tx,
        entered_rx,
    )
}

#[test]
fn pending_player_query_timeout_retains_driver_database_and_lifecycle_exclusion() {
    let harness = Harness::new(false);
    let owner = harness.operations.open_owner().unwrap();
    let mut game = GameLifecycle::new(harness.launch());
    let generation = game.start(|_| Ok(())).unwrap();
    game.publish().unwrap();
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut gate = Some((entered_tx, release_rx));
    let mut blocker = game
        .process
        .queue_list_guarded(generation, move || {
            let (entered, release) = gate.take().unwrap();
            entered.send(()).unwrap();
            release.recv_timeout(WAIT).unwrap();
            Err(StateError::new("state_owned_test_no_write"))
        })
        .unwrap();
    entered_rx.recv_timeout(WAIT).unwrap();
    let (shared, players, ticks) = DriverState::new(&owner, &game);
    let driver = FixtureDriver {
        driver: Driver {
            local_receiver: mpsc::sync_channel(1).1,
            owner,
            game,
            players,
            ticks,
            active: None,
            shared: shared.clone(),
            stop_wait: Duration::from_millis(25),
            observation_uncertain: None,
            after_dispatch: None,
        },
        cwd: harness.cwd.clone(),
        gate_release: release_tx.clone(),
    };
    let handle = thread::spawn(move || driver.run());
    let mut owned = OwnedRuntime {
        runtime: OperationRuntime {
            start_block: Default::default(),
            operations: harness.operations.clone(),
            shared: shared.clone(),
            local: LocalRuntime {
                wake: shared.wake.clone(),
                shared: shared.clone(),
                sender: mpsc::sync_channel(1).0,
            },
            driver: Some(handle),
        },
        cwd: harness.cwd.clone(),
        gate_release: Some(release_tx.clone()),
    };
    let error = owned
        .runtime
        .query_players(&harness.subject, generation, Duration::from_millis(250))
        .unwrap_err();
    assert!(matches!(error, super::super::PlayerQueryError::TimedOut));
    let error = owned
        .runtime
        .query_players(&harness.subject, generation, Duration::from_secs(1))
        .unwrap_err();
    assert!(
        matches!(error, super::super::PlayerQueryError::Busy),
        "timing out did not acknowledge the queued writer: {error:?}"
    );
    let start = harness.submit(GameCommand::Start);
    thread::sleep(Duration::from_millis(60));
    assert_eq!(
        harness.state(&start),
        super::super::OperationState::Queued,
        "lifecycle dispatch must remain excluded until the native query ACK"
    );
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    assert!(owned.runtime.driver.is_some());
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().generation,
        Some(generation)
    );
    assert_eq!(
        fs::read(harness.cwd.join("operation-commands")).unwrap(),
        b""
    );
    release_tx.send(()).unwrap();
    let mut ack = None;
    wait_for(|| {
        ack = blocker.try_result();
        ack.is_some()
    });
    assert!(matches!(
        ack,
        Some(Err(vesta_game_runtime::process::GuardedError::Guard(_)))
    ));
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert_eq!(
        fs::read(harness.cwd.join("operation-commands")).unwrap(),
        b""
    );
}

#[test]
fn pending_native_guard_shutdown_timeout_retains_exact_claim_lock_database_and_worker() {
    let harness = Harness::new(false);
    let (mut owned, stop, release, entered) = delayed_stop(&harness);
    entered.recv_timeout(WAIT).unwrap();
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    let facts = harness.facts(&stop);
    assert!(!facts.0);
    assert!(facts.1.is_some());
    assert_eq!(facts.2, 1);
    assert_eq!(harness.state(&stop), super::super::OperationState::Running);
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    assert!(owned.runtime.driver.is_some());
    let driver_thread = owned.runtime.driver.as_ref().unwrap().thread().id();
    // Admission has not produced an ACK. Wait beyond the configured 25ms stop
    // interval: elapsed time cannot fabricate callback return or release proof.
    thread::sleep(Duration::from_millis(60));
    assert_eq!(harness.facts(&stop), facts);
    assert_eq!(harness.state(&stop), super::super::OperationState::Running);
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    assert_eq!(
        owned.runtime.driver.as_ref().unwrap().thread().id(),
        driver_thread
    );
    release.send(()).unwrap();
    harness.wait_state(&stop, super::super::OperationState::Queued);
    assert!(!harness.facts(&stop).0);
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        ""
    );
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

#[test]
fn cancellation_before_delayed_native_gate_is_audited_cancelled_without_marker_or_stdin_effect() {
    let harness = Harness::new(false);
    let (mut owned, stop, release, entered) = delayed_stop(&harness);
    entered.recv_timeout(WAIT).unwrap();
    assert!(!harness.facts(&stop).0);
    assert!(matches!(
        harness
            .operations
            .request_cancel(&harness.subject, &stop)
            .unwrap(),
        super::super::CancelOutcome::Requested(_)
    ));
    release.send(()).unwrap();
    harness.wait_state(&stop, super::super::OperationState::Cancelled);
    assert_eq!(harness.facts(&stop), (false, None, 0, None));
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        ""
    );
    assert!(harness.operations.audit.verify_chain().unwrap().valid);
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

#[test]
fn cancellation_after_actual_stop_ack_cannot_fabricate_cancelled_native_work() {
    let harness = Harness::new(true);
    let mut owned = harness.runtime(Duration::from_secs(1));
    let generation = start(&harness, &owned.runtime);
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    wait_for(|| owned.runtime.game_snapshot().unwrap().phase == ProcessPhase::Stopping);
    assert!(harness.facts(&stop).0);
    assert!(matches!(
        harness
            .operations
            .request_cancel(&harness.subject, &stop)
            .unwrap(),
        super::super::CancelOutcome::Requested(_)
    ));
    harness.wait_state(&stop, super::super::OperationState::Failed);
    assert_eq!(
        harness.facts(&stop),
        (true, None, 0, Some("game_stop_timeout".into()))
    );
    assert_ne!(
        harness.state(&stop),
        super::super::OperationState::Cancelled
    );
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

struct AckDropGate {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl Drop for AckDropGate {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        // Disconnection releases panic cleanup; no timeout counts as success.
        let _ = self.release.recv_timeout(WAIT);
    }
}

fn delayed_stop_ack(
    harness: &Harness,
    inject_observation_failure: bool,
) -> (OwnedRuntime, OperationId, mpsc::Sender<()>) {
    let mut owner = harness.operations.open_owner().unwrap();
    let mut game = GameLifecycle::new(harness.launch());
    let generation = game.start(|_| Ok(())).unwrap();
    game.publish().unwrap();
    wait_for(|| harness.cwd.join("operation-ready").exists());
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: generation,
    });
    let claim = owner
        .claim_next_with_revision(&game.revision())
        .unwrap()
        .unwrap();
    owner.begin_run(&claim, &game.revision()).unwrap();
    let native = owner.native_effect_guard(&claim).unwrap();
    let view = game.view.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let ack_gate = AckDropGate {
        entered: entered_tx,
        release: release_rx,
    };
    let mut receipt = game
        .process
        .queue_stop_guarded(generation, move || {
            // Destruction of this owned capture happens after actual write_all but
            // before typed ACK; there is no production hook/provider injection.
            let _keep_until_callback_drop = &ack_gate;
            native.begin_effect(&view.revision()?)
        })
        .unwrap();
    entered_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(receipt.try_result(), None);
    assert!(harness.facts(&stop).0);
    let operations = owner.operations().clone();
    let (shared, players, ticks) = DriverState::new(&owner, &game);
    let mut driver = FixtureDriver {
        driver: Driver {
            local_receiver: mpsc::sync_channel(1).1,
            owner,
            game,
            players,
            ticks,
            active: Some(Active::StopWrite {
                claim,
                generation,
                receipt,
                restart: false,
            }),
            shared: shared.clone(),
            stop_wait: Duration::from_millis(25),
            observation_uncertain: None,
            after_dispatch: None,
        },
        cwd: harness.cwd.clone(),
        gate_release: release_tx.clone(),
    };
    if inject_observation_failure {
        // Deterministic private seam into the same production failure transition.
        driver.note_observation_failure(RuntimeError::ObserveFailed);
        assert_eq!(
            driver.game.view.revision().unwrap_err().code(),
            "source_revision_unavailable"
        );
    }
    let handle = thread::spawn(move || driver.run());
    let owned = OwnedRuntime {
        runtime: OperationRuntime {
            operations,
            start_block: Default::default(),
            shared: shared.clone(),
            local: LocalRuntime {
                wake: shared.wake.clone(),
                shared: shared.clone(),
                sender: mpsc::sync_channel(1).0,
            },
            driver: Some(handle),
        },
        cwd: harness.cwd.clone(),
        gate_release: Some(release_tx.clone()),
    };
    (owned, stop, release_tx)
}

fn dispatch_driver(harness: &Harness, probe: Option<fn(&mut Driver)>) -> FixtureDriver {
    let owner = harness.operations.open_owner().unwrap();
    let game = GameLifecycle::new(harness.launch());
    let (shared, players, ticks) = DriverState::new(&owner, &game);
    let (release, _) = mpsc::channel();
    FixtureDriver {
        driver: Driver {
            local_receiver: mpsc::sync_channel(1).1,
            owner,
            game,
            players,
            ticks,
            active: None,
            shared,
            stop_wait: Duration::from_millis(25),
            observation_uncertain: None,
            after_dispatch: probe,
        },
        cwd: harness.cwd.clone(),
        gate_release: release,
    }
}

#[test]
fn late_dispatch_observation_failure_survives_successful_later_polls_and_real_exit() {
    let harness = Harness::new(true);
    let start = harness.submit(GameCommand::Start);
    let driver = dispatch_driver(
        &harness,
        Some(|driver| {
            // Private test-only seam after a real dispatch, not a production fault
            // provider or claimed native syscall injection. It enters the same
            // sticky failure transition used by internal native poll refusals.
            driver.note_observation_failure(RuntimeError::ObserveFailed);
        }),
    );
    let operations = driver.owner.operations().clone();
    let shared = driver.shared.clone();
    let handle = thread::spawn(move || driver.run());
    let mut owned = OwnedRuntime {
        runtime: OperationRuntime {
            operations,
            start_block: Default::default(),
            shared: shared.clone(),
            local: LocalRuntime {
                wake: shared.wake.clone(),
                shared: shared.clone(),
                sender: mpsc::sync_channel(1).0,
            },
            driver: Some(handle),
        },
        cwd: harness.cwd.clone(),
        gate_release: None,
    };
    harness.wait_state(&start, super::super::OperationState::Succeeded);
    wait_for(|| owned.runtime.status().unwrap().last_error_code == Some("game_observe_failed"));
    wait_for(|| harness.cwd.join("operation-ready").is_file());
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Running
    );
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    assert!(harness.db.directory().acquire_runner_lock().is_err());
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    let error = owned.runtime.shutdown(WAIT).unwrap_err();
    assert!(matches!(
        error,
        OperationError::Runtime(RuntimeError::ObserveFailed)
    ));
    assert_eq!(error.code(), "game_observe_failed");
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().phase,
        ProcessPhase::Stopped
    );
    // An observation error after a known completed Start does not erase its fact.
    assert_eq!(
        harness.state(&start),
        super::super::OperationState::Succeeded
    );
    assert!(harness.db.directory().acquire_runner_lock().is_ok());
}

#[test]
fn merging_sticky_observation_failure_never_overwrites_an_earlier_failure() {
    let harness = Harness::new(false);
    let mut driver = dispatch_driver(&harness, None);
    driver.note_observation_failure(RuntimeError::ObserveFailed);
    let previous = OperationError::from(StateError::new("state_operation_corrupt"));
    let mut failure = Some(previous.clone());
    driver.retain_observation_failure(&mut failure);
    assert_eq!(failure, Some(previous));
    let mut failure = None;
    driver.retain_observation_failure(&mut failure);
    assert_eq!(
        failure,
        Some(OperationError::Runtime(RuntimeError::ObserveFailed))
    );
}

#[test]
fn native_observation_failure_before_later_ack_stays_uncertain_and_retains_claim_past_deadline() {
    let harness = Harness::new(true);
    let (mut owned, stop, release) = delayed_stop_ack(&harness, true);
    release.send(()).unwrap();
    wait_for(|| owned.runtime.game_snapshot().unwrap().phase == ProcessPhase::Stopping);
    thread::sleep(Duration::from_millis(60));
    let facts = harness.facts(&stop);
    assert!(facts.0);
    assert!(facts.1.is_some());
    assert_eq!(facts.2, 1);
    assert_eq!(harness.state(&stop), super::super::OperationState::Running);
    assert!(matches!(
        owned.runtime.shutdown(Duration::ZERO).unwrap(),
        ShutdownOutcome::TimedOut(_)
    ));
    assert_eq!(harness.db.close().unwrap_err().code(), "state_runtime_busy");
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    harness.wait_state(&stop, super::super::OperationState::RecoveryRequired);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap_err().code(),
        RuntimeError::ObserveFailed.code()
    );
    assert!(owned.runtime.driver.is_none());
    assert_eq!(harness.facts(&stop).2, 1);
}

#[test]
fn delayed_native_stdin_gate_rechecks_current_policy_and_refuses_revoked_stop_without_effect_marker()
 {
    let harness = Harness::new(false);
    let (mut owned, stop, release, entered) = delayed_stop(&harness);
    entered.recv_timeout(WAIT).unwrap();
    harness.deny_stop();
    release.send(()).unwrap();
    harness.wait_state(&stop, super::super::OperationState::Failed);
    let facts = harness.facts(&stop);
    assert!(!facts.0);
    assert!(facts.1.is_none());
    assert_eq!(facts.2, 0);
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        ""
    );
    assert!(owned.runtime.game_snapshot().unwrap().generation.is_some());
    fs::write(harness.cwd.join("release-game"), b"release").unwrap();
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
}

#[test]
fn explicit_restart_waits_for_exit_then_owns_distinct_generation() {
    let harness = Harness::new(false);
    let mut owned = harness.runtime(Duration::from_secs(1));
    let generation = start(&harness, &owned.runtime);
    let restart = harness.submit(GameCommand::Restart {
        expected_generation: generation,
    });
    harness.wait_state(&restart, super::super::OperationState::Succeeded);
    let next = owned.runtime.game_snapshot().unwrap().generation.unwrap();
    assert_ne!(generation, next);
    assert_eq!(harness.facts(&restart), (true, None, 0, None));
    assert_eq!(
        fs::read_to_string(harness.cwd.join("operation-commands")).unwrap(),
        "stop\n"
    );
    let stop = harness.submit(GameCommand::Stop {
        expected_generation: next,
    });
    harness.wait_state(&stop, super::super::OperationState::Succeeded);
    assert_eq!(
        owned.runtime.shutdown(WAIT).unwrap(),
        ShutdownOutcome::Complete
    );
    assert!(harness.operations.audit.verify_chain().unwrap().valid);
}

#[test]
fn explicit_restart_timeout_never_starts_a_second_generation() {
    let harness = Harness::new(true);
    let owned = harness.runtime(Duration::from_millis(25));
    let generation = start(&harness, &owned.runtime);
    let restart = harness.submit(GameCommand::Restart {
        expected_generation: generation,
    });
    harness.wait_state(&restart, super::super::OperationState::Failed);
    assert_eq!(
        owned.runtime.game_snapshot().unwrap().generation,
        Some(generation)
    );
    assert_eq!(
        harness.facts(&restart).3.as_deref(),
        Some("game_stop_timeout")
    );
}

#[path = "metrics_tests.rs"]
mod metrics_tests;
#[path = "moderation_tests.rs"]
mod moderation_tests;
#[path = "motd_tests.rs"]
mod motd_tests;
#[path = "sleep_tests.rs"]
mod sleep_tests;
#[path = "startup_whitelist_tests.rs"]
mod startup_whitelist_tests;
