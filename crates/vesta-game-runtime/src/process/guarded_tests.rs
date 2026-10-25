//! Inert native self-test executable only; never Java/Minecraft or external tools.
use super::*;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);
const ENABLE: &[u8] = b"vesta-owned-inert-guard-fixture-v1";

#[test]
fn inert_guarded_fixture() {
    if fs::read("guarded-fixture-enabled").ok().as_deref() != Some(ENABLE) {
        return;
    }
    let mut commands = OpenOptions::new()
        .create(true)
        .append(true)
        .open("guarded-commands")
        .unwrap();
    println!("guarded-fixture-ready");
    std::io::stdout().flush().unwrap();
    fs::write("guarded-ready", b"ready").unwrap();
    if std::path::Path::new("natural-exit").is_file() {
        return;
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        writeln!(commands, "{line}").unwrap();
        commands.flush().unwrap();
        if line == "stop" {
            return;
        }
    }
}
fn launch(cwd: &TempDir) -> LaunchSpec {
    fs::write(cwd.path().join("guarded-fixture-enabled"), ENABLE).unwrap();
    let executable = cwd.path().join(if cfg!(windows) {
        "owned-guard-fixture.exe"
    } else {
        "owned-guard-fixture"
    });
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    LaunchSpec::new(
        executable,
        vec![
            "--exact".into(),
            "process::guarded_tests::inert_guarded_fixture".into(),
            "--nocapture".into(),
        ],
        cwd.path().into(),
        GracefulStop::MinecraftCommand,
    )
    .unwrap()
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < until, "owned inert fixture timed out");
        thread::sleep(POLL_INTERVAL);
    }
}
fn receipt<E>(receipt: &mut GuardedInput<E>) -> Result<(), GuardedError<E>> {
    let mut result = None;
    wait_for(|| {
        result = receipt.try_result();
        result.is_some()
    });
    result.unwrap()
}
#[derive(Debug, Eq, PartialEq)]
enum Refusal {
    Revoked,
}

#[test]
fn generation_encoding_is_strict_nonzero_lowercase_lookup_only() {
    let generation = GenerationId([0xa5; 16]);
    assert_eq!(generation.to_hex(), "a5".repeat(16));
    assert_eq!(
        GenerationId::parse_hex(&generation.to_hex()),
        Ok(generation)
    );
    for invalid in [
        "0".repeat(32),
        "a5".repeat(15),
        "a5".repeat(17),
        "A5".repeat(16),
        "zz".repeat(16),
        "é".repeat(16),
    ] {
        assert_eq!(
            GenerationId::parse_hex(&invalid),
            Err(RuntimeError::StaleGeneration)
        );
    }
    assert!(!format!("{generation:?}").contains("a5"));
}

#[test]
fn guarded_start_preflight_and_denial_do_not_spawn_or_publish_generation() {
    let cwd = TempDir::new().unwrap();
    let spec = launch(&cwd);
    let calls = AtomicUsize::new(0);
    let mut process = GameProcess::new(ConsoleCapture::new());
    assert_eq!(
        process.start_guarded(&spec, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Refusal::Revoked)
        }),
        Err(GuardedError::Guard(Refusal::Revoked))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(process.snapshot().generation, None);
    assert!(!cwd.path().join("guarded-ready").exists());

    // The reusable literal host plan is re-preflighted before the guard.
    let removed = cwd.path().join("removed");
    fs::create_dir(&removed).unwrap();
    let invalid = LaunchSpec::new(
        std::env::current_exe().unwrap(),
        vec![],
        removed.clone(),
        GracefulStop::Unsupported,
    )
    .unwrap();
    fs::remove_dir(removed).unwrap();
    assert_eq!(
        process.start_guarded(&invalid, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<(), Refusal>(())
        }),
        Err(GuardedError::Runtime(RuntimeError::InvalidLaunch))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn delayed_writer_rechecks_fresh_guard_and_preserves_typed_refusal_without_stdin_effect() {
    let cwd = TempDir::new().unwrap();
    let spec = launch(&cwd);
    let mut process = GameProcess::new(ConsoleCapture::new());
    // Release channel is newer than process: unwind releases gate before joining.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let io = io::IoThreads::gated_stdin_for_test(&process.console, entered_tx, release_rx).unwrap();
    entered_rx.recv_timeout(WAIT).unwrap();
    let generation = process.new_generation().unwrap();
    process
        .own_spawned(
            spec.command().spawn().unwrap(),
            generation,
            spec.stop(),
            io,
            tree::ProcessTree::prepare().unwrap(),
        )
        .unwrap();
    wait_for(|| cwd.path().join("guarded-ready").exists());
    let allowed = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    let gate_allowed = allowed.clone();
    let gate_calls = calls.clone();
    let mut ack = process
        .queue_stop_guarded(generation, move || {
            gate_calls.fetch_add(1, Ordering::SeqCst);
            if gate_allowed.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(Refusal::Revoked)
            }
        })
        .unwrap();
    assert_eq!(ack.try_result(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Running);
    assert_eq!(
        process
            .owned
            .as_ref()
            .unwrap()
            .io
            .retained_workers_for_test(),
        3
    );
    allowed.store(false, Ordering::Release);
    release_tx.send(()).unwrap();
    assert_eq!(
        receipt(&mut ack),
        Err(GuardedError::Guard(Refusal::Revoked))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read_to_string(cwd.path().join("guarded-commands")).unwrap(),
        ""
    );
    assert_eq!(process.snapshot().generation, Some(generation));
    assert!(!process.owned.as_ref().unwrap().graceful_admitted);
    assert!(matches!(
        process.force_kill(generation, WAIT).unwrap(),
        WaitOutcome::Exited(_)
    ));
}

#[test]
fn actual_guarded_write_acknowledgement_and_join_are_distinct_from_queue_admission() {
    let cwd = TempDir::new().unwrap();
    let spec = launch(&cwd);
    let mut process = GameProcess::new(ConsoleCapture::new());
    let generation = process
        .start_guarded(&spec, |_| Ok::<(), Refusal>(()))
        .unwrap();
    wait_for(|| cwd.path().join("guarded-ready").exists());
    let calls = Arc::new(AtomicUsize::new(0));
    let gate_calls = calls.clone();
    let mut ack = process
        .queue_stop_guarded(generation, move || {
            gate_calls.fetch_add(1, Ordering::SeqCst);
            Ok::<(), Refusal>(())
        })
        .unwrap();
    assert_eq!(receipt(&mut ack), Ok(()));
    process.acknowledge_guarded_stop(generation).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        process.wait(generation, WAIT).unwrap(),
        WaitOutcome::Exited(_)
    ));
    assert_eq!(
        fs::read_to_string(cwd.path().join("guarded-commands")).unwrap(),
        "stop\n"
    );
    assert_eq!(process.snapshot().phase, ProcessPhase::Stopped);
    assert!(process.owned.is_none());
}

#[test]
fn naturally_exited_generation_is_no_effect_guarded_kill_not_synthetic_success() {
    let cwd = TempDir::new().unwrap();
    let spec = launch(&cwd);
    fs::write(cwd.path().join("natural-exit"), b"exit").unwrap();
    let mut process = GameProcess::new(ConsoleCapture::new());
    let generation = process
        .start_guarded(&spec, |_| Ok::<(), Refusal>(()))
        .unwrap();
    assert!(matches!(
        process.wait(generation, WAIT).unwrap(),
        WaitOutcome::Exited(_)
    ));
    let calls = AtomicUsize::new(0);
    assert_eq!(
        process.force_kill_guarded(generation, Duration::ZERO, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<(), Refusal>(())
        }),
        Err(GuardedError::Runtime(RuntimeError::StaleGeneration))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Preserve existing trusted unguarded observation semantics.
    assert!(matches!(
        process.force_kill(generation, Duration::ZERO).unwrap(),
        WaitOutcome::Exited(_)
    ));
}

#[test]
fn guarded_list_delivers_only_literal_list_without_marking_stopping() {
    let cwd = TempDir::new().unwrap();
    let mut process = GameProcess::new(ConsoleCapture::new());
    let generation = process.start(launch(&cwd)).unwrap();
    wait_for(|| cwd.path().join("guarded-ready").exists());
    let mut ack = process
        .queue_list_guarded(generation, || Ok::<(), Refusal>(()))
        .unwrap();
    assert_eq!(receipt(&mut ack), Ok(()));
    wait_for(|| fs::read_to_string(cwd.path().join("guarded-commands")).unwrap() == "list\n");
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Running);
    assert!(!process.owned.as_ref().unwrap().graceful_admitted);
    assert!(matches!(
        process.force_kill(generation, WAIT).unwrap(),
        WaitOutcome::Exited(_)
    ));
    assert!(
        process
            .queue_list_guarded(generation, || Ok::<(), Refusal>(()))
            .is_err()
    );
}

#[test]
fn delayed_list_writer_refuses_revocation_without_command_or_phase_change() {
    let cwd = TempDir::new().unwrap();
    let spec = launch(&cwd);
    let mut process = GameProcess::new(ConsoleCapture::new());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let io = io::IoThreads::gated_stdin_for_test(&process.console, entered_tx, release_rx).unwrap();
    entered_rx.recv_timeout(WAIT).unwrap();
    let generation = process.new_generation().unwrap();
    process
        .own_spawned(
            spec.command().spawn().unwrap(),
            generation,
            spec.stop(),
            io,
            tree::ProcessTree::prepare().unwrap(),
        )
        .unwrap();
    wait_for(|| cwd.path().join("guarded-ready").exists());
    let allowed = Arc::new(AtomicBool::new(true));
    let gate_allowed = allowed.clone();
    let mut ack = process
        .queue_list_guarded(generation, move || {
            if gate_allowed.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(Refusal::Revoked)
            }
        })
        .unwrap();
    assert_eq!(ack.try_result(), None);
    assert_eq!(
        process
            .owned
            .as_ref()
            .unwrap()
            .io
            .retained_workers_for_test(),
        3
    );
    allowed.store(false, Ordering::Release);
    release_tx.send(()).unwrap();
    assert_eq!(
        receipt(&mut ack),
        Err(GuardedError::Guard(Refusal::Revoked))
    );
    assert_eq!(
        fs::read_to_string(cwd.path().join("guarded-commands")).unwrap(),
        ""
    );
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Running);
    assert!(matches!(
        process.force_kill(generation, WAIT).unwrap(),
        WaitOutcome::Exited(_)
    ));
}

#[test]
fn guarded_list_stale_or_stopping_admission_never_invokes_the_guard() {
    let cwd = TempDir::new().unwrap();
    let mut process = GameProcess::new(ConsoleCapture::new());
    let generation = process.start(launch(&cwd)).unwrap();
    wait_for(|| cwd.path().join("guarded-ready").exists());
    for unavailable in [GenerationId([0; 16]), generation] {
        if unavailable == generation {
            // Set only owned phase evidence, without issuing an arbitrary command.
            process.owned.as_mut().unwrap().stopping = true;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        assert!(matches!(
            process.queue_list_guarded(unavailable, move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok::<(), Refusal>(())
            }),
            Err(RuntimeError::StaleGeneration)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    assert_eq!(
        fs::read_to_string(cwd.path().join("guarded-commands")).unwrap(),
        ""
    );
    assert!(matches!(
        process.force_kill(generation, WAIT).unwrap(),
        WaitOutcome::Exited(_)
    ));
}

#[test]
fn guarded_kill_denial_and_stale_preflight_do_not_kill_current_generation() {
    let cwd = TempDir::new().unwrap();
    let spec = launch(&cwd);
    let mut process = GameProcess::new(ConsoleCapture::new());
    let generation = process
        .start_guarded(&spec, |_| Ok::<(), Refusal>(()))
        .unwrap();
    wait_for(|| cwd.path().join("guarded-ready").exists());
    let calls = AtomicUsize::new(0);
    assert_eq!(
        process.force_kill_guarded(generation, Duration::ZERO, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Refusal::Revoked)
        }),
        Err(GuardedError::Guard(Refusal::Revoked))
    );
    let stale = GenerationId(if generation == GenerationId([1; 16]) {
        [2; 16]
    } else {
        [1; 16]
    });
    assert_eq!(
        process.force_kill_guarded(stale, Duration::ZERO, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<(), Refusal>(())
        }),
        Err(GuardedError::Runtime(RuntimeError::StaleGeneration))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Running);
    assert!(matches!(
        process
            .force_kill_guarded(generation, WAIT, |_| Ok::<(), Refusal>(()))
            .unwrap(),
        WaitOutcome::Exited(_)
    ));
}
