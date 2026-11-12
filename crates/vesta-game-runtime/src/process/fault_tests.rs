//! Private fault seams and a narrowly filtered inert copy of this unit-test executable.
use super::*;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, Write},
    process::{Command, Stdio},
};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);

#[test]
fn inert_child_fixture() {
    // Ordinary unit-test invocation is inert; only the controlled subprocess opts in.
    if std::env::var_os("VESTA_RUNTIME_UNIT_FIXTURE").is_none() {
        return;
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open("unit-commands")
        .unwrap();
    println!("unit-fixture-ready");
    eprintln!("unit-fixture-stderr-ready");
    std::io::stdout().flush().unwrap();
    fs::write("unit-ready", b"ready").unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        writeln!(log, "{line}").unwrap();
        log.flush().unwrap();
        match line.as_str() {
            "stop" => return,
            "flood" => {
                let text = "x".repeat(4096);
                for _ in 0..512 {
                    println!("{text}");
                    eprintln!("{text}");
                }
                std::io::stdout().flush().unwrap();
                std::io::stderr().flush().unwrap();
                fs::write("unit-flood-complete", b"done").unwrap();
            }
            _ => (),
        }
    }
}

fn child(cwd: &TempDir) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    tree::ProcessTree::configure(&mut command);
    command
        .args([
            "--exact",
            "process::fault_tests::inert_child_fixture",
            "--nocapture",
        ])
        .env("VESTA_RUNTIME_UNIT_FIXTURE", "1")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_ready(cwd: &TempDir) {
    let until = Instant::now() + WAIT;
    while !cwd.path().join("unit-ready").is_file() {
        assert!(Instant::now() < until, "unit subprocess startup timed out");
        thread::sleep(POLL_INTERVAL);
    }
}

fn exit(outcome: WaitOutcome) -> ExitSummary {
    let WaitOutcome::Exited(exit) = outcome else {
        panic!("unit subprocess exit timed out");
    };
    exit
}

#[test]
fn failed_native_kill_retains_ownership_and_does_not_suppress_graceful_recovery() {
    // Cwd precedes owner in local declaration order, so owner drops first on unwind.
    let cwd = TempDir::new().unwrap();
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    let io = io::IoThreads::prepare(&runtime.console).unwrap();
    let generation = runtime.new_generation().unwrap();
    runtime
        .own_spawned(
            child(&cwd),
            generation,
            GracefulStop::MinecraftCommand,
            io,
            tree::ProcessTree::prepare().unwrap(),
        )
        .unwrap();
    wait_ready(&cwd);
    assert_eq!(
        runtime.kill_with(generation, Duration::ZERO, |_| Err(
            RuntimeError::KillFailed
        )),
        Err(RuntimeError::KillFailed)
    );
    let snapshot = runtime.poll().unwrap();
    assert_eq!(snapshot.phase, ProcessPhase::Stopping);
    assert_eq!(snapshot.generation, Some(generation));
    assert_eq!(snapshot.last_exit, None);
    assert!(!runtime.owned.as_ref().unwrap().graceful_admitted);
    let result = exit(runtime.stop(generation, WAIT).unwrap());
    assert_eq!(result.code, Some(0));
    assert_eq!(
        fs::read_to_string(cwd.path().join("unit-commands")).unwrap(),
        "stop\n"
    );
}

#[test]
fn failed_second_pipe_dispatch_retains_undelivered_handles_and_reports_uncertainty() {
    let cwd = TempDir::new().unwrap();
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    let mut io = io::IoThreads::prepare(&runtime.console).unwrap();
    io.disconnect_stdout_for_test();
    let generation = runtime.new_generation().unwrap();
    assert_eq!(
        runtime.own_spawned(
            child(&cwd),
            generation,
            GracefulStop::MinecraftCommand,
            io,
            tree::ProcessTree::prepare().unwrap()
        ),
        Err(RuntimeError::StartupUncertain)
    );
    wait_ready(&cwd);
    let owned = runtime.owned.as_ref().unwrap();
    assert!(
        owned.child.stdin.is_none(),
        "first dispatch transferred actual stdin ownership"
    );
    assert!(
        owned.child.stdout.is_some(),
        "failed-send stdout returned to exact child"
    );
    assert!(
        owned.child.stderr.is_some(),
        "unattempted stderr never left exact child"
    );
    let snapshot = runtime.poll().unwrap();
    assert_eq!(snapshot.phase, ProcessPhase::Stopping);
    assert_eq!(snapshot.generation, Some(generation));
    assert!(snapshot.output_failed);
    let spec = LaunchSpec::new(
        std::env::current_exe().unwrap(),
        vec![],
        cwd.path().into(),
        GracefulStop::Unsupported,
    )
    .unwrap();
    assert_eq!(runtime.start(spec), Err(RuntimeError::AlreadyOwned));
    let result = exit(runtime.stop(generation, WAIT).unwrap());
    assert_eq!(result.code, Some(0));
    assert!(
        result.output_failed,
        "undrained output is not successful capture"
    );
    assert_eq!(
        fs::read_to_string(cwd.path().join("unit-commands")).unwrap(),
        "stop\n"
    );
}

#[test]
fn missing_first_pipe_never_extracts_or_drops_the_unattempted_output_pipes() {
    let cwd = TempDir::new().unwrap();
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    let io = io::IoThreads::prepare(&runtime.console).unwrap();
    let generation = runtime.new_generation().unwrap();
    let mut child = child(&cwd);
    let stdin = child.stdin.take().unwrap();
    assert_eq!(
        runtime.own_spawned(
            child,
            generation,
            GracefulStop::MinecraftCommand,
            io,
            tree::ProcessTree::prepare().unwrap()
        ),
        Err(RuntimeError::StartupUncertain)
    );
    wait_ready(&cwd);
    let owned = runtime.owned.as_ref().unwrap();
    assert!(owned.child.stdout.is_some() && owned.child.stderr.is_some());
    assert_eq!(owned.generation, generation);
    let result = exit(runtime.force_kill(generation, WAIT).unwrap());
    assert!(!result.success);
    assert!(result.output_failed && result.stdin_failed);
    drop(stdin);
}

#[test]
fn poisoned_console_still_drains_actual_pipes_and_child_processes_a_later_stop() {
    let cwd = TempDir::new().unwrap();
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    let io = io::IoThreads::prepare(&runtime.console).unwrap();
    let generation = runtime.new_generation().unwrap();
    runtime
        .own_spawned(
            child(&cwd),
            generation,
            GracefulStop::MinecraftCommand,
            io,
            tree::ProcessTree::prepare().unwrap(),
        )
        .unwrap();
    wait_ready(&cwd);
    runtime.console.poison_for_test();
    runtime.queue_line(generation, "flood").unwrap();
    // Processing stop requires writing 2 MiB to EACH actual pipe first.
    let result = exit(runtime.stop(generation, WAIT).unwrap());
    assert_eq!(result.code, Some(0));
    assert!(result.output_failed);
    assert!(cwd.path().join("unit-flood-complete").is_file());
    assert_eq!(
        fs::read_to_string(cwd.path().join("unit-commands")).unwrap(),
        "flood\nstop\n"
    );
}

#[test]
fn rng_failure_and_zero_or_recent_collision_never_create_a_generation() {
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    let mut calls = 0;
    assert_eq!(
        runtime.generation_with(|_| {
            calls += 1;
            Err(RuntimeError::GenerationUnavailable)
        }),
        Err(RuntimeError::GenerationUnavailable)
    );
    assert_eq!(calls, 1);
    calls = 0;
    assert_eq!(
        runtime.generation_with(|_| {
            calls += 1;
            Ok(())
        }),
        Err(RuntimeError::GenerationUnavailable)
    );
    assert_eq!(calls, 8);
    let previous = GenerationId([1; 16]);
    runtime.last_exit = Some(ExitSummary {
        generation: previous,
        success: true,
        code: Some(0),
        output_failed: false,
        stdin_failed: false,
    });
    calls = 0;
    let next = runtime
        .generation_with(|bytes| {
            calls += 1;
            bytes.fill(if calls == 8 { 2 } else { 1 });
            Ok(())
        })
        .unwrap();
    assert_eq!(calls, 8);
    assert_ne!(next, previous);
    assert_eq!(runtime.snapshot().generation, None);
}

#[test]
fn observe_failure_retains_exact_live_generation_and_all_workers_until_recovery() {
    let cwd = TempDir::new().unwrap();
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    let io = io::IoThreads::prepare(&runtime.console).unwrap();
    let generation = runtime.new_generation().unwrap();
    runtime
        .own_spawned(
            child(&cwd),
            generation,
            GracefulStop::MinecraftCommand,
            io,
            tree::ProcessTree::prepare().unwrap(),
        )
        .unwrap();
    wait_ready(&cwd);
    assert_eq!(
        runtime
            .owned
            .as_ref()
            .unwrap()
            .io
            .retained_workers_for_test(),
        3
    );
    assert_eq!(
        runtime.poll_with(|_| Err(RuntimeError::ObserveFailed)),
        Err(RuntimeError::ObserveFailed)
    );
    let owned = runtime.owned.as_ref().unwrap();
    assert_eq!(owned.generation, generation);
    assert!(owned.exit.is_none());
    assert_eq!(owned.io.retained_workers_for_test(), 3);
    let spec = LaunchSpec::new(
        std::env::current_exe().unwrap(),
        vec![],
        cwd.path().into(),
        GracefulStop::Unsupported,
    )
    .unwrap();
    assert_eq!(runtime.start(spec), Err(RuntimeError::AlreadyOwned));
    assert_eq!(runtime.poll().unwrap().generation, Some(generation));
    assert_eq!(exit(runtime.stop(generation, WAIT).unwrap()).code, Some(0));
    assert_eq!(
        fs::read_to_string(cwd.path().join("unit-commands")).unwrap(),
        "stop\n"
    );
}

#[test]
fn full_stdin_queue_refuses_stop_without_admission_or_phase_change_then_recovers() {
    let cwd = TempDir::new().unwrap();
    let mut runtime = GameProcess::new(ConsoleCapture::new());
    // Channels follow owner in declaration order: panic drops release before owner joins.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let io = io::IoThreads::gated_stdin_for_test(&runtime.console, entered_tx, release_rx).unwrap();
    entered_rx
        .recv_timeout(WAIT)
        .expect("writer actually entered controlled gate");
    let generation = runtime.new_generation().unwrap();
    runtime
        .own_spawned(
            child(&cwd),
            generation,
            GracefulStop::MinecraftCommand,
            io,
            tree::ProcessTree::prepare().unwrap(),
        )
        .unwrap();
    wait_ready(&cwd);
    for index in 0..8 {
        runtime
            .queue_line(generation, &format!("queued-{index}"))
            .unwrap();
    }
    assert_eq!(
        runtime.stop(generation, Duration::ZERO),
        Err(RuntimeError::StdinBusy)
    );
    let owned = runtime.owned.as_ref().unwrap();
    assert!(!owned.graceful_admitted && !owned.stopping);
    assert_eq!(runtime.poll().unwrap().phase, ProcessPhase::Running);
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let log = fs::read_to_string(cwd.path().join("unit-commands")).unwrap_or_default();
        if log.lines().count() == 8 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "released writer did not actually deliver queued lines"
        );
        thread::sleep(POLL_INTERVAL);
    }
    assert_eq!(exit(runtime.stop(generation, WAIT).unwrap()).code, Some(0));
    let expected = (0..8)
        .map(|index| format!("queued-{index}\n"))
        .collect::<String>()
        + "stop\n";
    assert_eq!(
        fs::read_to_string(cwd.path().join("unit-commands")).unwrap(),
        expected
    );
}
