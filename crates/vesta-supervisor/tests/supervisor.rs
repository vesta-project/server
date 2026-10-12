//! Supervisor lifecycle over its real protected endpoint, with owned inert copies of
//! this test executable as the game. No Minecraft, Java, services or network.
use std::{
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use vesta_game_runtime::process::{
    GracefulStop, GuardedError, LaunchSpec, ProcessPhase, RuntimeError, WaitOutcome,
};
use vesta_state::StateDirectory;
use vesta_supervisor::{
    RemoteProcess, SupervisorClient, SupervisorConfig, native, record,
    wire::{Op, Reply},
};

const WAIT: Duration = Duration::from_secs(20);
const ENABLE: &[u8] = b"vesta-owned-inert-supervisor-fixture-v1";
const IDENTITY: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[test]
fn inert_supervised_game() {
    if fs::read("supervisor-fixture-enabled").ok().as_deref() != Some(ENABLE) {
        return;
    }
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if line.map(|line| sender.send(line)).is_err() {
                break;
            }
        }
    });
    let mut commands = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("commands.txt")
        .unwrap();
    println!("inert-supervised-ready");
    std::io::stdout().flush().unwrap();
    if Path::new("crash-on-start").exists() {
        std::process::exit(23);
    }
    loop {
        if Path::new("release-game").exists() {
            std::process::exit(0);
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(line) => {
                writeln!(commands, "{line}").unwrap();
                commands.flush().unwrap();
                println!("echo:{line}");
                std::io::stdout().flush().unwrap();
                if line == "stop" {
                    std::process::exit(0);
                }
                if line == "crash" {
                    std::process::exit(23);
                }
                // Minecraft-style save acknowledgements, unless told to stay silent.
                if !Path::new("silent-save").exists() {
                    let info = |message: &str| {
                        println!("[12:00:00] [Server thread/INFO]: {message}");
                    };
                    match line.as_str() {
                        "save-off" if Path::new("autosave-off").exists() => {
                            info("Saving is already turned off")
                        }
                        "save-off" => info("Automatic saving is now disabled"),
                        "save-all flush" => {
                            if Path::new("delay-flush").exists() {
                                thread::sleep(Duration::from_secs(11));
                            }
                            info("Saving the game (this may take a moment!)");
                            info("Saved the game");
                        }
                        "save-on" => info("Automatic saving is now enabled"),
                        _ => {}
                    }
                    std::io::stdout().flush().unwrap();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Stdin closed (supervisor gone): keep running, like a real server would.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

struct Fixture {
    root: tempfile::TempDir,
    state: StateDirectory,
    game: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("inert game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("supervisor-fixture-enabled"), ENABLE).unwrap();
        let state = StateDirectory::new(root.path().join("private state"), Some(&game)).unwrap();
        state.prepare().unwrap();
        Self { root, state, game }
    }
    fn launch(&self) -> LaunchSpec {
        let executable = self.game.join(if cfg!(windows) {
            "owned-supervised-fixture.exe"
        } else {
            "owned-supervised-fixture"
        });
        if !executable.exists() {
            fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        }
        LaunchSpec::new(
            executable,
            vec![
                "--exact".into(),
                "inert_supervised_game".into(),
                "--nocapture".into(),
            ],
            self.game.clone(),
            GracefulStop::MinecraftCommand,
        )
        .unwrap()
    }
    fn release(&self) {
        fs::write(self.game.join("release-game"), b"release").unwrap();
    }
    fn commands(&self) -> String {
        fs::read_to_string(self.game.join("commands.txt")).unwrap_or_default()
    }
}

struct Running {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<vesta_supervisor::Result<()>>>,
}
impl Running {
    fn start(fixture: &Fixture) -> Self {
        Self::start_with(fixture, None)
    }
    fn start_with(fixture: &Fixture, restart: Option<vesta_supervisor::RestartPolicy>) -> Self {
        let config = SupervisorConfig {
            state: fixture.state.clone(),
            launch: fixture.launch(),
            launch_identity: IDENTITY.into(),
            restart,
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(vesta_supervisor::run(config, async {
                let _ = stopped.await;
            }))
        });
        let client = SupervisorClient::new(&fixture.state).unwrap();
        let until = Instant::now() + WAIT;
        while vesta_supervisor::probe(&client).ok().flatten().is_none() {
            assert!(Instant::now() < until, "supervisor answers");
            assert!(!thread.is_finished(), "supervisor exited during start");
            thread::sleep(Duration::from_millis(20));
        }
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
    fn join(mut self) -> vesta_supervisor::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.thread.take().unwrap().join().unwrap()
    }
}

fn client(fixture: &Fixture) -> Arc<SupervisorClient> {
    Arc::new(SupervisorClient::new(&fixture.state).unwrap())
}

fn eventually(mut check: impl FnMut() -> bool, what: &str) {
    let until = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn console_contains(remote: &mut RemoteProcess, text: &str) -> bool {
    let _ = remote.poll();
    let console = remote.console();
    let mut after = 0;
    loop {
        let page = console.snapshot(after, 200).unwrap();
        if page.lines.iter().any(|line| line.text.contains(text)) {
            return true;
        }
        if page.lines.is_empty() {
            return false;
        }
        after = page.next_cursor;
    }
}

fn wait_receipt<E: std::fmt::Debug>(
    mut receipt: vesta_game_runtime::process::GuardedInput<E>,
) -> Result<(), GuardedError<E>> {
    let until = Instant::now() + WAIT;
    loop {
        if let Some(result) = receipt.try_result() {
            return result;
        }
        assert!(Instant::now() < until, "receipt resolves");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn game_survives_agent_disconnect_and_is_adopted_without_a_duplicate_launch() {
    let fixture = Fixture::new();
    let running = Running::start(&fixture);
    let mut first = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    let generation = first.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    eventually(
        || console_contains(&mut first, "inert-supervised-ready"),
        "game ready",
    );
    // The agent goes away entirely; the supervisor keeps owning and draining the game.
    drop(first);
    let directory = fixture.state.supervisor().unwrap();
    let stored = record::read(&directory)
        .unwrap()
        .expect("generation record");
    assert_eq!(stored.generation, generation.to_hex());
    assert_eq!(stored.liveness(), native::Liveness::Alive);

    let mut second = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    assert_eq!(second.snapshot().generation, Some(generation));
    assert_eq!(second.snapshot().phase, ProcessPhase::Running);
    // Output produced while no agent was attached is still in the supervisor ring.
    assert!(console_contains(&mut second, "inert-supervised-ready"));
    assert_eq!(
        second.start_guarded(|_| Ok::<(), ()>(())),
        Err(GuardedError::Runtime(RuntimeError::AlreadyOwned))
    );

    let receipt = second
        .queue_command_guarded(generation, "say supervised", || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    eventually(
        || fixture.commands().contains("say supervised"),
        "command delivered",
    );
    eventually(
        || console_contains(&mut second, "echo:say supervised"),
        "echo mirrored",
    );

    // Shutdown is refused while the game is running.
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert_eq!(
        raw.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::Refused {
            code: "supervisor_game_running".into()
        }
    );

    let receipt = second
        .queue_stop_guarded(generation, || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    second.acknowledge_guarded_stop(generation).unwrap();
    eventually(
        || {
            second
                .poll()
                .is_ok_and(|s| s.generation.is_none() && s.last_exit.is_some())
        },
        "game exit observed",
    );
    assert_eq!(second.snapshot().last_exit.unwrap().generation, generation);
    eventually(
        || record::read(&directory).unwrap().is_none(),
        "record cleared after exit",
    );
    assert_eq!(fixture.commands().matches("stop").count(), 1);
    drop(second);
    assert_eq!(
        raw.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::ShuttingDown
    );
    running.join().unwrap();
}

#[test]
fn guard_refusal_sends_nothing_and_kill_is_explicit() {
    let fixture = Fixture::new();
    let running = Running::start(&fixture);
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    assert_eq!(
        remote.start_guarded(|_| Err::<(), _>("denied")),
        Err(GuardedError::Guard("denied"))
    );
    assert_eq!(remote.poll().unwrap().generation, None);
    let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    eventually(
        || console_contains(&mut remote, "inert-supervised-ready"),
        "game ready",
    );
    let receipt = remote
        .queue_command_guarded(generation, "never-sent", || Err::<(), _>("denied"))
        .unwrap();
    assert_eq!(wait_receipt(receipt), Err(GuardedError::Guard("denied")));
    assert!(!fixture.commands().contains("never-sent"));
    match remote
        .force_kill_guarded(generation, Duration::from_secs(10), |_| Ok::<(), ()>(()))
        .unwrap()
    {
        WaitOutcome::Exited(exit) => assert_eq!(exit.generation, generation),
        WaitOutcome::TimedOut(_) => panic!("kill observed within the bound"),
    }
    assert_eq!(remote.poll().unwrap().generation, None);
    drop(remote);
    fixture.release();
    running.join().unwrap();
}

#[test]
fn mismatched_launch_identity_and_second_supervisor_refuse() {
    let fixture = Fixture::new();
    let running = Running::start(&fixture);
    assert_eq!(
        RemoteProcess::attach(client(&fixture), "2".repeat(64)).err(),
        Some("supervisor_launch_mismatch")
    );
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert_eq!(
        raw.call(
            Op::Start {
                launch_identity: "2".repeat(64)
            },
            Duration::from_secs(5)
        )
        .unwrap(),
        Reply::Refused {
            code: "supervisor_launch_mismatch".into()
        }
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let second = runtime.block_on(vesta_supervisor::run(
        SupervisorConfig {
            state: fixture.state.clone(),
            launch: fixture.launch(),
            launch_identity: IDENTITY.into(),
            restart: None,
        },
        std::future::pending(),
    ));
    assert_eq!(second, Err("supervisor_already_running"));
    running.join().unwrap();
}

#[test]
fn surviving_recorded_game_blocks_launch_until_it_exits() {
    let fixture = Fixture::new();
    // A game left behind by a supervisor that crashed: alive, recorded, no stdin.
    let mut orphan = std::process::Command::new(fixture.launch_program())
        .args(["--exact", "inert_supervised_game", "--nocapture"])
        .current_dir(&fixture.game)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = orphan.id();
    let directory = fixture.state.supervisor().unwrap();
    record::write(
        &directory,
        &record::GenerationRecord {
            version: 1,
            generation: "0123456789abcdef0123456789abcdef".into(),
            pid,
            process_start: native::process_start(pid).unwrap(),
            boot_id: native::boot_id(),
            launch_identity: IDENTITY.into(),
        },
    )
    .unwrap();
    let running = Running::start(&fixture);
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    assert!(remote.orphan_running());
    assert_eq!(
        SupervisorClient::new(&fixture.state)
            .unwrap()
            .call(Op::Hold { lease_ms: 60_000 }, Duration::from_secs(2))
            .unwrap(),
        Reply::Refused {
            code: "supervisor_hold_not_stopped".into()
        }
    );
    assert_eq!(
        remote.start_guarded(|_| Ok::<(), ()>(())),
        Err(GuardedError::Runtime(RuntimeError::AlreadyOwned))
    );
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert_eq!(
        raw.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::Refused {
            code: "supervisor_game_running".into()
        }
    );
    assert!(vesta_supervisor::instance_busy(&fixture.state).unwrap());
    // Nothing killed it. Once it exits on its own, launching is allowed again.
    assert!(orphan.try_wait().unwrap().is_none());
    fixture.release();
    orphan.wait().unwrap();
    eventually(
        || remote.poll().is_ok() && !remote.orphan_running(),
        "orphan exit observed",
    );
    assert!(record::read(&directory).unwrap().is_none());
    fs::remove_file(fixture.game.join("release-game")).unwrap();
    let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    assert_eq!(remote.poll().unwrap().generation, Some(generation));
    drop(remote);
    fixture.release();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    running.join().unwrap();
    let _ = &fixture.root;
}

impl Fixture {
    fn launch_program(&self) -> PathBuf {
        let _ = self.launch();
        self.game.join(if cfg!(windows) {
            "owned-supervised-fixture.exe"
        } else {
            "owned-supervised-fixture"
        })
    }
}

#[test]
fn unresolved_launch_intent_blocks_start_until_explicitly_resolved() {
    let fixture = Fixture::new();
    // A crash between intent and record: a child may exist that nothing can identify.
    let directory = fixture.state.supervisor().unwrap();
    record::write_intent(
        &directory,
        &record::LaunchIntent {
            version: 1,
            launch_identity: IDENTITY.into(),
        },
    )
    .unwrap();
    assert!(vesta_supervisor::instance_busy(&fixture.state).unwrap());
    let running = Running::start(&fixture);
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    assert!(remote.launch_unresolved());
    assert_eq!(
        SupervisorClient::new(&fixture.state)
            .unwrap()
            .call(Op::Hold { lease_ms: 60_000 }, Duration::from_secs(2))
            .unwrap(),
        Reply::Refused {
            code: "supervisor_hold_not_stopped".into()
        }
    );
    assert_eq!(
        remote.start_guarded(|_| Ok::<(), ()>(())),
        Err(GuardedError::Runtime(RuntimeError::AlreadyOwned))
    );
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert_eq!(
        raw.call(
            Op::Start {
                launch_identity: IDENTITY.into()
            },
            Duration::from_secs(5)
        )
        .unwrap(),
        Reply::Refused {
            code: "supervisor_launch_unresolved".into()
        }
    );
    assert_eq!(
        raw.call(Op::ResolveLaunch, Duration::from_secs(2)).unwrap(),
        Reply::Resolved
    );
    assert!(record::read_intent(&directory).unwrap().is_none());
    let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    // A successful launch leaves only the generation record, never a stale intent.
    assert!(record::read_intent(&directory).unwrap().is_none());
    assert_eq!(
        record::read(&directory).unwrap().unwrap().generation,
        generation.to_hex()
    );
    assert_eq!(
        raw.call(Op::ResolveLaunch, Duration::from_secs(2)).unwrap(),
        Reply::Refused {
            code: "supervisor_nothing_to_resolve".into()
        }
    );
    drop(remote);
    fixture.release();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    // Accepted shutdown closes admission: no later start can be admitted.
    assert!(matches!(
        raw.call(
            Op::Start {
                launch_identity: IDENTITY.into()
            },
            Duration::from_secs(5)
        ),
        Ok(Reply::Refused { .. }) | Err(_)
    ));
    running.join().unwrap();
}

#[test]
fn cancelling_the_supervisor_never_kills_a_live_game() {
    let fixture = Fixture::new();
    let config = SupervisorConfig {
        state: fixture.state.clone(),
        launch: fixture.launch(),
        launch_identity: IDENTITY.into(),
        restart: None,
    };
    let (started, ready) = mpsc::channel();
    let state = fixture.state.clone();
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let serving = vesta_supervisor::run(config, std::future::pending());
            tokio::pin!(serving);
            // Start a game, then drop the supervisor future mid-flight.
            let starter = tokio::task::spawn_blocking(move || {
                let client = Arc::new(SupervisorClient::new(&state).unwrap());
                let until = Instant::now() + WAIT;
                let mut remote = loop {
                    if let Ok(remote) = RemoteProcess::attach(client.clone(), IDENTITY.into()) {
                        break remote;
                    }
                    assert!(Instant::now() < until, "supervisor answers");
                    thread::sleep(Duration::from_millis(20));
                };
                remote.start_guarded(|_| Ok::<(), ()>(())).unwrap()
            });
            tokio::select! {
                _ = &mut serving => panic!("supervisor exited"),
                generation = starter => started.send(generation.unwrap()).unwrap(),
            }
            // `serving` is dropped here without completing: cancellation.
        });
    });
    let generation = ready.recv_timeout(WAIT).unwrap();
    worker.join().unwrap();
    let stored = record::read(&fixture.state.supervisor().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.generation, generation.to_hex());
    // Still running after its owner was cancelled and dropped.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(stored.liveness(), native::Liveness::Alive);
    fixture.release();
    eventually(
        || stored.liveness() == native::Liveness::Gone,
        "released game exits",
    );
}

#[test]
fn player_list_is_refused_while_supervisor_output_is_unmirrored() {
    let fixture = Fixture::new();
    let running = Running::start(&fixture);
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    eventually(
        || console_contains(&mut remote, "inert-supervised-ready"),
        "game ready",
    );
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    // A stale horizon: the supervisor has output after cursor 0, so nothing is written.
    assert_eq!(
        raw.call(
            Op::List {
                generation: generation.to_hex(),
                expect_latest: 0
            },
            Duration::from_secs(15)
        )
        .unwrap(),
        Reply::Refused {
            code: "supervisor_console_unmirrored".into()
        }
    );
    assert!(!fixture.commands().contains("list"));
    // Through the mirror, the horizon is current and the write happens.
    let receipt = remote
        .queue_list_guarded(generation, || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    eventually(|| fixture.commands().contains("list"), "list delivered");
    drop(remote);
    fixture.release();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    running.join().unwrap();
}

#[test]
fn retained_client_handles_cannot_exhaust_the_endpoint() {
    use std::io::{Read, Write as _};
    let fixture = Fixture::new();
    let running = Running::start(&fixture);
    let endpoint = native::endpoint(
        fixture.state.path(),
        &fixture
            .state
            .path()
            .join(vesta_state::artifacts::SUPERVISOR_DIR),
    )
    .unwrap();
    let hello = vesta_supervisor::wire::encode_request(Op::Hello).unwrap();
    // Each raw client completes one request but never closes its handle.
    let mut retained = Vec::new();
    for _ in 0..12 {
        let until = Instant::now() + WAIT;
        let mut pipe = loop {
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&endpoint)
            {
                Ok(pipe) => break pipe,
                Err(_) => {
                    assert!(Instant::now() < until, "endpoint accepts again");
                    thread::sleep(Duration::from_millis(20));
                }
            }
        };
        pipe.write_all(&hello).unwrap();
        let mut length = [0; 4];
        pipe.read_exact(&mut length).unwrap();
        retained.push(pipe);
    }
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert!(matches!(
        raw.call(Op::Hello, Duration::from_secs(2)).unwrap(),
        Reply::Hello(_)
    ));
    drop(retained);
    running.join().unwrap();
}

fn health(raw: &SupervisorClient) -> vesta_supervisor::wire::Health {
    match raw
        .call(Op::Sync { after: 0 }, Duration::from_secs(2))
        .unwrap()
    {
        Reply::Sync(page) => page.health,
        other => panic!("unexpected reply {other:?}"),
    }
}

fn fast_policy() -> vesta_supervisor::RestartPolicy {
    vesta_supervisor::RestartPolicy::new(
        3,
        Duration::from_secs(600),
        Duration::from_millis(100),
        Duration::from_millis(400),
    )
    .unwrap()
}

#[test]
fn crash_restarts_with_backoff_but_stops_and_clean_exits_do_not() {
    let fixture = Fixture::new();
    let running = Running::start_with(&fixture, Some(fast_policy()));
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    let first = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    eventually(
        || console_contains(&mut remote, "inert-supervised-ready"),
        "game ready",
    );
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert!(health(&raw).restart_enabled);
    // Crash: the supervisor restarts it as a new generation after the backoff.
    let receipt = remote
        .queue_command_guarded(first, "crash", || Ok::<(), ()>(()))
        .unwrap();
    let _ = wait_receipt(receipt);
    eventually(
        || {
            remote
                .poll()
                .is_ok_and(|s| s.generation.is_some_and(|g| g != first))
        },
        "automatic restart",
    );
    let second = remote.snapshot().generation.unwrap();
    assert_eq!(health(&raw).recent_crashes, 1);
    // A graceful stop is not a crash: nothing restarts.
    let receipt = remote
        .queue_stop_guarded(second, || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    eventually(
        || remote.poll().is_ok_and(|s| s.generation.is_none()),
        "stopped",
    );
    thread::sleep(Duration::from_millis(800));
    assert_eq!(remote.poll().unwrap().generation, None);
    let after_stop = health(&raw);
    assert_eq!(after_stop.recent_crashes, 1);
    assert_eq!(after_stop.restart_pending_ms, None);
    assert!(!after_stop.crash_loop);
    drop(remote);
    fixture.release();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    running.join().unwrap();
}

#[test]
fn crash_loop_breaker_leaves_the_game_stopped_until_reset_even_across_restarts() {
    let fixture = Fixture::new();
    fs::write(fixture.game.join("crash-on-start"), b"crash").unwrap();
    let running = Running::start_with(&fixture, Some(fast_policy()));
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    eventually(|| health(&raw).crash_loop, "breaker trips");
    let tripped = health(&raw);
    assert_eq!(tripped.recent_crashes, 3);
    assert_eq!(tripped.restart_pending_ms, None);
    thread::sleep(Duration::from_millis(800));
    assert_eq!(remote.poll().unwrap().generation, None);
    drop(remote);
    // The suspension survives a supervisor restart.
    assert_eq!(
        raw.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::ShuttingDown
    );
    running.join().unwrap();
    let running = Running::start_with(&fixture, Some(fast_policy()));
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert!(health(&raw).crash_loop);
    assert_eq!(
        raw.call(Op::ResetRestarts, Duration::from_secs(2)).unwrap(),
        Reply::RestartsReset
    );
    let reset = health(&raw);
    assert!(!reset.crash_loop);
    assert_eq!(reset.recent_crashes, 0);
    assert_eq!(
        raw.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::ShuttingDown
    );
    running.join().unwrap();
}

#[test]
fn explicit_actions_and_configuration_govern_scheduled_restarts() {
    let fixture = Fixture::new();
    // A long backoff keeps the scheduled restart observable.
    let slow = vesta_supervisor::RestartPolicy::new(
        3,
        Duration::from_secs(600),
        Duration::from_secs(30),
        Duration::from_secs(60),
    )
    .unwrap();
    let running = Running::start_with(&fixture, Some(slow));
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    let crash = |remote: &mut RemoteProcess| {
        let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
        eventually(
            || console_contains(remote, "inert-supervised-ready"),
            "game ready",
        );
        let receipt = remote
            .queue_command_guarded(generation, "crash", || Ok::<(), ()>(()))
            .unwrap();
        let _ = wait_receipt(receipt);
        generation
    };
    let first = crash(&mut remote);
    eventually(
        || health(&raw).restart_pending_ms.is_some(),
        "restart scheduled",
    );
    // A refused kill for a stale generation does not cancel recovery.
    assert!(matches!(
        raw.call(
            Op::Kill {
                generation: first.to_hex(),
                wait_ms: 0
            },
            Duration::from_secs(5)
        )
        .unwrap(),
        Reply::Refused { .. }
    ));
    assert!(health(&raw).restart_pending_ms.is_some());
    // An explicit start supersedes the scheduled restart.
    let manual = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    let after_start = health(&raw);
    assert_eq!(after_start.restart_pending_ms, None);
    assert_eq!(after_start.recent_crashes, 1);
    assert_eq!(remote.poll().unwrap().generation, Some(manual));
    eventually(
        || console_contains(&mut remote, "inert-supervised-ready"),
        "manual game ready",
    );
    let receipt = remote
        .queue_command_guarded(manual, "crash", || Ok::<(), ()>(()))
        .unwrap();
    let _ = wait_receipt(receipt);
    eventually(
        || health(&raw).restart_pending_ms.is_some(),
        "restart scheduled again",
    );
    // The agent's configuration can disable restart on a running supervisor.
    remote.configure(None).unwrap();
    let disabled = health(&raw);
    assert!(!disabled.restart_enabled);
    assert_eq!(disabled.restart_pending_ms, None);
    remote.configure(Some(slow)).unwrap();
    assert!(health(&raw).restart_enabled);
    drop(remote);
    fixture.release();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    running.join().unwrap();
}

fn command_lines(fixture: &Fixture) -> Vec<String> {
    fixture.commands().lines().map(str::to_owned).collect()
}

fn started(
    fixture: &Fixture,
) -> (
    Running,
    RemoteProcess,
    vesta_game_runtime::process::GenerationId,
) {
    let running = Running::start(fixture);
    let mut remote = RemoteProcess::attach(client(fixture), IDENTITY.into()).unwrap();
    let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    eventually(
        || console_contains(&mut remote, "inert-supervised-ready"),
        "game ready",
    );
    (running, remote, generation)
}

fn finish(fixture: &Fixture, running: Running, remote: RemoteProcess) {
    drop(remote);
    fixture.release();
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    running.join().unwrap();
}

#[test]
fn save_lease_suspends_flushes_and_restores_only_prior_autosave() {
    let fixture = Fixture::new();
    let (running, remote, generation) = started(&fixture);
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    let (token, restore) = remote
        .save_suspend(generation, Duration::from_secs(60), None)
        .unwrap();
    assert!(restore);
    assert!(health(&raw).save_lease_ms.is_some_and(|ms| ms > 50_000));
    let lines = command_lines(&fixture);
    assert_eq!(lines, ["save-off", "save-all flush"]);
    // A second lease is refused; renewal works; shutdown waits for the lease.
    assert!(
        remote
            .save_suspend(generation, Duration::from_secs(60), None)
            .is_err()
    );
    remote
        .save_renew(generation, &token, Duration::from_secs(120))
        .unwrap();
    assert!(health(&raw).save_lease_ms.is_some_and(|ms| ms > 100_000));
    assert!(matches!(
        raw.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::Refused { .. }
    ));
    assert!(remote.save_resume(generation, &token).unwrap());
    eventually(
        || {
            command_lines(&fixture)
                .last()
                .is_some_and(|l| l == "save-on")
        },
        "autosave restored",
    );
    assert_eq!(health(&raw).save_lease_ms, None);

    // An owner's deliberate save-off is respected: nothing re-enables autosave.
    fs::write(fixture.game.join("autosave-off"), b"off").unwrap();
    let (second_token, restore) = remote
        .save_suspend(generation, Duration::from_secs(60), None)
        .unwrap();
    assert!(!restore);
    assert_ne!(token, second_token);
    assert!(remote.save_resume(generation, &token).is_err());
    assert!(
        remote
            .save_renew(generation, &token, Duration::from_secs(60))
            .is_err()
    );
    assert!(!remote.save_resume(generation, &second_token).unwrap());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        command_lines(&fixture)
            .iter()
            .filter(|l| l.as_str() == "save-on")
            .count(),
        1
    );
    finish(&fixture, running, remote);
}

#[test]
fn unacknowledged_save_off_keeps_unknown_obligation_until_exit() {
    let fixture = Fixture::new();
    let (running, mut remote, generation) = started(&fixture);
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    fs::write(fixture.game.join("silent-save"), b"silent").unwrap();
    assert!(
        remote
            .save_suspend(generation, Duration::from_secs(10), Some(true))
            .is_err()
    );
    let status = health(&raw);
    assert!(status.save_state_unknown);
    assert!(status.save_lease_ms.is_some());
    assert!(
        vesta_supervisor::save::read(&fixture.state.supervisor().unwrap())
            .unwrap()
            .unwrap()
            .unresolved
    );
    assert!(!command_lines(&fixture).contains(&"save-on".to_owned()));
    fs::remove_file(fixture.game.join("silent-save")).unwrap();
    assert!(
        remote
            .save_suspend(generation, Duration::from_secs(60), None)
            .is_err()
    );
    let receipt = remote
        .queue_stop_guarded(generation, || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    eventually(
        || remote.poll().is_ok_and(|s| s.generation.is_none()),
        "stopped",
    );
    eventually(
        || !health(&raw).save_state_unknown,
        "unknown cleared on exit",
    );
    let next = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    let (_, restore) = remote
        .save_suspend(next, Duration::from_secs(10), None)
        .unwrap();
    assert!(restore);
    eventually(
        || {
            command_lines(&fixture)
                .last()
                .is_some_and(|l| l == "save-on")
        },
        "expiry restores",
    );
    assert_eq!(health(&raw).save_lease_ms, None);
    finish(&fixture, running, remote);
}

#[test]
fn a_lease_never_sends_cleanup_to_another_generation() {
    let fixture = Fixture::new();
    let (running, mut remote, first) = started(&fixture);
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    assert!(
        remote
            .save_suspend(first, Duration::from_secs(60), None)
            .unwrap()
            .1
    );
    // The game exits while the lease is held: the lease ends with nothing sent.
    let receipt = remote
        .queue_stop_guarded(first, || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    eventually(
        || remote.poll().is_ok_and(|s| s.generation.is_none()),
        "stopped",
    );
    eventually(|| health(&raw).save_lease_ms.is_none(), "lease cleared");
    let second = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    assert_ne!(second, first);
    thread::sleep(Duration::from_millis(300));
    assert!(!command_lines(&fixture).contains(&"save-on".to_owned()));
    finish(&fixture, running, remote);
}

#[test]
fn a_lease_on_an_orphan_is_reported_stranded_until_it_exits() {
    let fixture = Fixture::new();
    let mut orphan = std::process::Command::new(fixture.launch_program())
        .args(["--exact", "inert_supervised_game", "--nocapture"])
        .current_dir(&fixture.game)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = orphan.id();
    let directory = fixture.state.supervisor().unwrap();
    let generation = "0123456789abcdef0123456789abcdef";
    record::write(
        &directory,
        &record::GenerationRecord {
            version: 1,
            generation: generation.into(),
            pid,
            process_start: native::process_start(pid).unwrap(),
            boot_id: native::boot_id(),
            launch_identity: IDENTITY.into(),
        },
    )
    .unwrap();
    vesta_supervisor::save::write(
        &directory,
        &vesta_supervisor::save::SaveLease {
            version: 1,
            token: "test".into(),
            unresolved: false,
            ending: false,
            generation: generation.into(),
            expires_at_ms: 0,
            restore_autosave: true,
        },
    )
    .unwrap();
    let running = Running::start(&fixture);
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    let stranded = health(&raw);
    assert!(stranded.save_lease_stranded);
    assert_eq!(stranded.save_lease_ms, None);
    fixture.release();
    orphan.wait().unwrap();
    eventually(
        || !health(&raw).save_lease_stranded,
        "orphan exit clears lease",
    );
    assert!(vesta_supervisor::save::read(&directory).unwrap().is_none());
    fs::remove_file(fixture.game.join("release-game")).unwrap();
    eventually(
        || {
            raw.call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "supervisor idle",
    );
    running.join().unwrap();
}

#[test]
fn expiry_during_flush_never_reports_suspend_success() {
    let fixture = Fixture::new();
    let (running, remote, generation) = started(&fixture);
    fs::write(fixture.game.join("delay-flush"), b"delay").unwrap();
    assert!(
        remote
            .save_suspend(generation, Duration::from_secs(10), None)
            .is_err()
    );
    eventually(
        || command_lines(&fixture).iter().any(|line| line == "save-on"),
        "expiry restoration delivered",
    );
    assert_eq!(
        health(&SupervisorClient::new(&fixture.state).unwrap()).save_lease_ms,
        None
    );
    finish(&fixture, running, remote);
}

#[test]
fn save_window_fences_other_save_commands_and_generation_change_aborts_wait() {
    let fixture = Fixture::new();
    let (running, mut remote, generation) = started(&fixture);
    fs::write(fixture.game.join("silent-save"), b"silent").unwrap();
    let raw = client(&fixture);
    let worker = {
        let raw = raw.clone();
        thread::spawn(move || {
            vesta_supervisor::save::suspend(&raw, generation, Duration::from_secs(60), None)
        })
    };
    eventually(
        || command_lines(&fixture).contains(&"save-off".to_string()),
        "save-off admitted",
    );
    for line in ["save-all", "/SAVE-ALL flush", " Save-Off ", "/sAvE-oN"] {
        assert_eq!(
            raw.call(
                Op::Command {
                    generation: generation.to_hex(),
                    line: line.into()
                },
                Duration::from_secs(2)
            )
            .unwrap(),
            Reply::Refused {
                code: "supervisor_save_window_active".into()
            }
        );
    }
    raw.call(
        Op::Command {
            generation: generation.to_hex(),
            line: "say fine".into(),
        },
        Duration::from_secs(2),
    )
    .unwrap();
    let now = Instant::now();
    let receipt = remote
        .queue_stop_guarded(generation, || Ok::<(), ()>(()))
        .unwrap();
    wait_receipt(receipt).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(
        now.elapsed() < Duration::from_secs(3),
        "generation change aborts immediately"
    );
    eventually(
        || remote.poll().is_ok_and(|s| s.generation.is_none()),
        "stopped",
    );
    assert!(!command_lines(&fixture).contains(&"save-on".to_string()));
    finish(&fixture, running, remote);
}

#[test]
fn stopped_hold_fences_start_and_stale_tokens() {
    let fixture = Fixture::new();
    let running = Running::start(&fixture);
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    let token = vesta_supervisor::save::hold(&raw, Duration::from_secs(60)).unwrap();
    assert!(health(&raw).maintenance_hold);
    assert_eq!(
        raw.call(
            Op::Start {
                launch_identity: IDENTITY.into()
            },
            Duration::from_secs(2)
        )
        .unwrap(),
        Reply::Refused {
            code: "supervisor_maintenance_hold".into()
        }
    );
    assert!(vesta_supervisor::save::release_hold(&raw, "wrong").is_err());
    vesta_supervisor::save::renew_hold(&raw, &token, Duration::from_secs(120)).unwrap();
    vesta_supervisor::save::release_hold(&raw, &token).unwrap();
    let next = vesta_supervisor::save::hold(&raw, Duration::from_secs(60)).unwrap();
    assert_ne!(token, next);
    assert!(vesta_supervisor::save::release_hold(&raw, &token).is_err());
    vesta_supervisor::save::release_hold(&raw, &next).unwrap();
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    assert_eq!(
        raw.call(Op::Hold { lease_ms: 60_000 }, Duration::from_secs(2))
            .unwrap(),
        Reply::Refused {
            code: "supervisor_hold_not_stopped".into()
        }
    );
    finish(&fixture, running, remote);
}

#[test]
fn hold_keeps_crash_restart_pending_until_expiry() {
    let fixture = Fixture::new();
    let policy = vesta_supervisor::RestartPolicy::new(
        3,
        Duration::from_secs(60),
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    let running = Running::start_with(&fixture, Some(policy));
    let raw = SupervisorClient::new(&fixture.state).unwrap();
    let mut remote = RemoteProcess::attach(client(&fixture), IDENTITY.into()).unwrap();
    let generation = remote.start_guarded(|_| Ok::<(), ()>(())).unwrap();
    raw.call(
        Op::Command {
            generation: generation.to_hex(),
            line: "crash".into(),
        },
        Duration::from_secs(2),
    )
    .unwrap();
    eventually(
        || health(&raw).restart_pending_ms.is_some(),
        "restart pending",
    );
    let token = vesta_supervisor::save::hold(&raw, Duration::from_secs(10)).unwrap();
    thread::sleep(Duration::from_secs(3));
    assert!(health(&raw).maintenance_hold);
    assert!(health(&raw).restart_pending_ms.is_some());
    assert!(vesta_supervisor::save::hold(&raw, Duration::from_secs(60)).is_err());
    eventually(|| !health(&raw).maintenance_hold, "hold expired");
    eventually(
        || {
            remote
                .poll()
                .is_ok_and(|s| s.generation.is_some_and(|g| g != generation))
        },
        "pending restart fires",
    );
    assert!(vesta_supervisor::save::release_hold(&raw, &token).is_err());
    finish(&fixture, running, remote);
}
