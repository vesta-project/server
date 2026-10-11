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
        let config = SupervisorConfig {
            state: fixture.state.clone(),
            launch: fixture.launch(),
            launch_identity: IDENTITY.into(),
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
