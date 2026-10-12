//! End-to-end supervision with the real `vesta-supervisor` executable and a real host:
//! agent exit leaves the game running, a new agent adopts it without relaunching, and
//! a supervisor crash leaves a recorded game that blocks any duplicate launch.
//! The game is an owned inert copy of this test executable. No Minecraft or Java.
use std::{
    ffi::OsStr,
    fs,
    io::BufRead,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use vesta_control::{
    config::HostConfiguration,
    host::{Host, HostShutdown, supervised_launch},
    operations::LocalAction,
};
use vesta_state::SystemClock;
use vesta_supervisor::{
    RemoteProcess, SupervisorClient, record,
    wire::{Op, Reply},
};

const MARKER: &[u8] = b"owned-inert-supervisor-e2e-v1";
const WAIT: Duration = Duration::from_secs(30);

#[test]
fn inert_e2e_game() {
    if fs::read("inert-e2e").ok().as_deref() != Some(MARKER) {
        return;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    // A real server keeps running when its supervisor's pipes close; so does this one.
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "inert-e2e-ready");
        let _ = writeln!(
            out,
            "[12:00:00] [Server thread/INFO]: Done (0.123s)! For help, type \"help\""
        );
        let _ = out.flush();
    }
    loop {
        if Path::new("release-game").exists() {
            std::process::exit(0);
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(line) if line == "stop" => std::process::exit(0),
            _ => {}
        }
    }
}

fn eventually(mut check: impl FnMut() -> bool, what: &str) {
    let until = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(25));
    }
}

struct Layout {
    _temp: tempfile::TempDir,
    root: PathBuf,
    host: PathBuf,
    game: PathBuf,
}
impl Layout {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let game = root.join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("inert-e2e"), MARKER).unwrap();
        let executable = if cfg!(windows) { "inert.exe" } else { "inert" };
        fs::copy(std::env::current_exe().unwrap(), game.join(executable)).unwrap();
        fs::write(
            root.join("launch.toml"),
            format!(
                "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '{executable}'\nargs = ['--exact', 'inert_e2e_game', '--nocapture']\nstop = 'console-stop'\n"
            ),
        )
        .unwrap();
        let host = root.join("host.toml");
        fs::write(
            &host,
            "schema-version = 1\n[paths]\nstate = 'state'\ngame = 'game'\nscan-roots = ['game']\nlaunch-definition = 'launch.toml'\n[pack]\nname = 'Supervised'\nversion = '1.0.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n[process]\nsupervision = 'separate'\n",
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            host,
            game,
        }
    }
    fn config(&self) -> HostConfiguration {
        HostConfiguration::load_file(&self.host, &self.root).unwrap()
    }
    fn spawn_supervisor(&self) -> (std::process::Child, Arc<SupervisorClient>) {
        let launch = supervised_launch(&self.config()).unwrap();
        launch.state.prepare().unwrap();
        let client = Arc::new(SupervisorClient::new(&launch.state).unwrap());
        let (child, _) = vesta_supervisor::spawn_detached(
            &client,
            Path::new(env!("CARGO_BIN_EXE_vesta-supervisor")),
            &[
                OsStr::new("--host"),
                self.host.as_os_str(),
                OsStr::new("--base"),
                self.root.as_os_str(),
            ],
            // The test harness may itself run inside a no-breakaway job.
            vesta_supervisor::Containment::AllowContained,
        )
        .unwrap();
        (child, client)
    }
    fn open_host(&self, client: Arc<SupervisorClient>) -> Host {
        let config = self.config();
        let identity = supervised_launch(&config).unwrap().identity;
        let remote = RemoteProcess::attach(client, identity).unwrap();
        Host::open_with(config, Arc::new(SystemClock), Some(remote)).unwrap()
    }
    fn record(&self) -> Option<record::GenerationRecord> {
        let launch = supervised_launch(&self.config()).unwrap();
        record::read(&launch.state.supervisor().unwrap()).unwrap()
    }
}

fn close(host: Host) {
    match host.shutdown(Duration::from_secs(20)).unwrap() {
        HostShutdown::Complete => {}
        HostShutdown::TimedOut(_) => panic!("agent shutdown must not wait for a supervised game"),
    }
}

#[test]
fn agent_exit_keeps_the_game_and_a_supervisor_crash_blocks_duplicates() {
    let layout = Layout::new();
    // Supervision is opt-in and must be satisfied by an attached supervisor.
    assert_eq!(
        Host::open(layout.config(), Arc::new(SystemClock))
            .err()
            .map(|e| e.code()),
        Some("host_supervisor_required")
    );
    let (mut supervisor, client) = layout.spawn_supervisor();
    let host = layout.open_host(client.clone());
    host.runtime()
        .local_handle()
        .request(LocalAction::Start)
        .unwrap();
    let generation = host.runtime().game_snapshot().unwrap().generation.unwrap();
    // Readiness flows from the supervisor's console mirror; without server.properties
    // there is no probe target, so readiness is log-only and says so.
    let status = host.runtime().network_handle();
    eventually(
        || {
            status.readiness().is_ok_and(|view| {
                view.readiness == vesta_control::operations::readiness::Readiness::Ready
                    && !view.probed
            })
        },
        "log-only readiness",
    );
    let health = status.supervision().unwrap().expect("supervised health");
    assert!(!health.restart_enabled);
    assert!(!health.crash_loop);
    let recorded = layout.record().expect("generation recorded");
    assert_eq!(recorded.generation, generation.to_hex());
    assert!(recorded.held());

    // Agent exits: the game and supervisor keep running.
    close(host);
    assert!(layout.record().unwrap().held());
    assert!(supervisor.try_wait().unwrap().is_none());

    // A new agent adopts the same generation and never launches another.
    let host = layout.open_host(client.clone());
    let status = host.runtime().network_handle();
    eventually(
        || host.runtime().game_snapshot().unwrap().generation == Some(generation),
        "adopted generation",
    );
    assert_eq!(
        host.runtime().local_handle().request(LocalAction::Start),
        Err("local_start_refused")
    );
    host.runtime()
        .local_handle()
        .request(LocalAction::Stop)
        .unwrap();
    eventually(|| layout.record().is_none(), "record cleared after stop");
    host.runtime()
        .local_handle()
        .request(LocalAction::Start)
        .unwrap();
    let second = host.runtime().game_snapshot().unwrap().generation.unwrap();
    assert_ne!(second, generation);
    // Let the new game finish starting (its output is mirrored) before the crash.
    eventually(
        || {
            status.readiness().is_ok_and(|view| {
                view.readiness == vesta_control::operations::readiness::Readiness::Ready
            })
        },
        "second generation ready",
    );

    // Supervisor crash: the game survives with its record, and is never killed.
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    let orphan = layout.record().expect("record survives supervisor crash");
    assert!(orphan.held());
    // The agent lost its supervisor: shutdown completes, reporting lost observation
    // if the driver noticed in time. It never waits on, stops or kills the game.
    match host.shutdown(Duration::from_secs(20)) {
        Ok(HostShutdown::Complete) => {}
        Err(error) => assert_eq!(error.code(), "game_observe_failed"),
        Ok(HostShutdown::TimedOut(_)) => panic!("shutdown must not wait for a lost supervisor"),
    }

    let (mut replacement, client) = layout.spawn_supervisor();
    let remote = RemoteProcess::attach(
        client.clone(),
        supervised_launch(&layout.config()).unwrap().identity,
    )
    .unwrap();
    assert!(remote.orphan_running());
    drop(remote);
    let launch = supervised_launch(&layout.config()).unwrap();
    assert!(vesta_supervisor::instance_busy(&launch.state).unwrap());
    // Switching the same server back to in-process mode cannot launch beside it.
    let in_process = layout.root.join("host-in-process.toml");
    fs::write(
        &in_process,
        fs::read_to_string(&layout.host)
            .unwrap()
            .replace("[process]\nsupervision = 'separate'\n", ""),
    )
    .unwrap();
    assert_eq!(
        Host::open(
            HostConfiguration::load_file(&in_process, &layout.root).unwrap(),
            Arc::new(SystemClock)
        )
        .err()
        .map(|e| e.code()),
        Some("host_supervised_game_running")
    );
    // An attachment bound to a different launch identity is never accepted.
    assert_eq!(
        RemoteProcess::attach(client.clone(), "0".repeat(64)).err(),
        Some("supervisor_launch_mismatch")
    );
    assert_eq!(
        client.call(Op::Shutdown, Duration::from_secs(2)).unwrap(),
        Reply::Refused {
            code: "supervisor_game_running".into()
        }
    );

    // Only the game's own exit clears the block.
    fs::write(layout.game.join("release-game"), b"release").unwrap();
    eventually(|| !orphan.held(), "orphan exits");
    eventually(
        || {
            client
                .call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "idle supervisor shuts down",
    );
    eventually(
        || replacement.try_wait().unwrap().is_some(),
        "supervisor exit",
    );
    assert!(layout.record().is_none());
}

#[test]
fn start_on_agent_start_launches_once_and_never_duplicates() {
    let layout = Layout::new();
    let text = fs::read_to_string(&layout.host).unwrap();
    fs::write(
        &layout.host,
        text.replace(
            "supervision = 'separate'\n",
            "supervision = 'separate'\nstart-on-agent-start = true\n",
        ),
    )
    .unwrap();
    let (mut supervisor, client) = layout.spawn_supervisor();
    let host = layout.open_host(client.clone());
    let status = host.runtime().network_handle();
    eventually(
        || status.game_snapshot().unwrap().generation.is_some(),
        "started on agent start",
    );
    let generation = status.game_snapshot().unwrap().generation.unwrap();
    eventually(
        || {
            status.readiness().is_ok_and(|view| {
                view.readiness == vesta_control::operations::readiness::Readiness::Ready
            })
        },
        "game ready",
    );
    close(host);
    // A second agent start finds the running generation and starts nothing.
    let host = layout.open_host(client.clone());
    let status = host.runtime().network_handle();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(status.game_snapshot().unwrap().generation, Some(generation));
    assert_eq!(layout.record().unwrap().generation, generation.to_hex());
    host.runtime()
        .local_handle()
        .request(LocalAction::Stop)
        .unwrap();
    close(host);
    eventually(
        || {
            client
                .call(Op::Shutdown, Duration::from_secs(2))
                .is_ok_and(|reply| reply == Reply::ShuttingDown)
        },
        "idle supervisor shuts down",
    );
    eventually(
        || supervisor.try_wait().unwrap().is_some(),
        "supervisor exit",
    );
}
