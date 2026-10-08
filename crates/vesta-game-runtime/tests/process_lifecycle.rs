//! Public lifecycle assertions using only the inert native fixture, never Minecraft.
mod common;
use common::*;
use std::{fs, time::Duration};
use vesta_game_runtime::{
    console::{ConsoleStream, MAX_LINE_BYTES},
    process::{GracefulStop, LaunchSpec, MAX_WAIT, ProcessPhase, RuntimeError, WaitOutcome},
};

fn fixture(mode: &str, stop: GracefulStop) -> Fixture {
    Fixture::new(vec![mode.into()], stop)
}

#[test]
fn graceful_stop_delivers_exact_stop_and_drains_final_output() {
    let mut fixture = fixture("graceful", GracefulStop::MinecraftCommand);
    let exit = fixture.stop();
    assert_eq!(exit.generation, fixture.generation);
    assert!(exit.success);
    assert_eq!(exit.code, Some(0));
    assert!(!exit.output_failed && !exit.stdin_failed);
    assert_eq!(fixture.recorded(1), ["stop"]);
    let lines = all_lines(&fixture.console);
    assert_eq!(
        lines
            .iter()
            .rfind(|line| line.stream == ConsoleStream::Stdout)
            .unwrap()
            .text,
        "fixture-stopped"
    );
    assert_eq!(
        lines
            .iter()
            .rfind(|line| line.stream == ConsoleStream::Stderr)
            .unwrap()
            .text,
        "fixture-stderr-ready"
    );
    let snapshot = fixture.process.poll().unwrap();
    assert_eq!(snapshot.phase, ProcessPhase::Stopped);
    assert_eq!(snapshot.generation, None);
    assert_eq!(snapshot.last_exit, Some(exit));
    assert_eq!(
        exited(
            fixture
                .process
                .wait(fixture.generation, Duration::ZERO)
                .unwrap()
        ),
        exit
    );
}

#[test]
fn actual_crash_exit_is_not_a_capture_failure_or_success() {
    let mut fixture = fixture("graceful", GracefulStop::MinecraftCommand);
    fixture
        .process
        .queue_line(fixture.generation, "crash")
        .unwrap();
    let exit = exited(fixture.process.wait(fixture.generation, DEADLINE).unwrap());
    assert_eq!(fixture.recorded(1), ["crash"]);
    assert_eq!(exit.code, Some(23));
    assert!(!exit.success && !exit.output_failed);
    assert_eq!(fixture.process.poll().unwrap().last_exit, Some(exit));
}

#[test]
fn ignored_stop_timeout_keeps_live_owned_stopping_generation_and_refuses_restart() {
    let mut fixture = fixture("ignore-stop", GracefulStop::MinecraftCommand);
    let outcome = fixture
        .process
        .stop(fixture.generation, Duration::from_millis(20))
        .unwrap();
    let WaitOutcome::TimedOut(snapshot) = outcome else {
        panic!("ignored stop exited");
    };
    assert_eq!(snapshot.phase, ProcessPhase::Stopping);
    assert_eq!(snapshot.generation, Some(fixture.generation));
    assert_eq!(snapshot.last_exit, None);
    wait_line(&fixture.console, ConsoleStream::Stdout, "stop-ignored");
    let replacement = LaunchSpec::new(
        program(),
        vec!["graceful".into()],
        fixture.dir.path().into(),
        GracefulStop::MinecraftCommand,
    )
    .unwrap();
    assert_eq!(
        fixture.process.start(replacement),
        Err(RuntimeError::AlreadyOwned)
    );
    // Commands still work after timeout: a real live-child witness, not merely flags.
    fixture
        .process
        .queue_line(fixture.generation, "alive-after-timeout")
        .unwrap();
    wait_line(
        &fixture.console,
        ConsoleStream::Stdout,
        "echo:alive-after-timeout",
    );
    assert_eq!(fixture.recorded(2), ["stop", "alive-after-timeout"]);
    assert_eq!(
        fs::read_to_string(fixture.dir.path().join("starts.txt")).unwrap(),
        "start\n"
    );
    let exit = exited(
        fixture
            .process
            .force_kill(fixture.generation, DEADLINE)
            .unwrap(),
    );
    assert!(!exit.success);
    assert_eq!(fixture.process.poll().unwrap().phase, ProcessPhase::Stopped);
}

#[test]
fn slow_stop_requires_explicit_release_and_repeated_stop_does_not_enqueue_again() {
    let mut fixture = fixture("slow-stop", GracefulStop::MinecraftCommand);
    for _ in 0..2 {
        let WaitOutcome::TimedOut(snapshot) = fixture
            .process
            .stop(fixture.generation, Duration::from_millis(20))
            .unwrap()
        else {
            panic!("unreleased fixture cannot exit");
        };
        assert_eq!(snapshot.phase, ProcessPhase::Stopping);
        assert_eq!(snapshot.generation, Some(fixture.generation));
        assert_eq!(snapshot.last_exit, None);
    }
    wait_line(&fixture.console, ConsoleStream::Stdout, "stop-pending");
    assert!(!fixture.dir.path().join("release-stop").exists());
    assert_eq!(fixture.recorded(1), ["stop"]);
    fs::write(fixture.dir.path().join("release-stop"), b"release").unwrap();
    let exit = exited(fixture.process.wait(fixture.generation, DEADLINE).unwrap());
    assert!(exit.success);
    assert_eq!(exit.code, Some(0));
    assert_eq!(fixture.recorded(1), ["stop"]);
    assert!(
        all_lines(&fixture.console)
            .iter()
            .any(|line| line.text == "fixture-stopped")
    );
}

#[test]
fn stale_generation_cannot_command_stop_kill_or_wait_on_same_owner_replacement() {
    let mut fixture = fixture("graceful", GracefulStop::MinecraftCommand);
    let stale = fixture.generation;
    let first = fixture.stop();
    let next_dir = tempfile::tempdir().unwrap();
    let launch = LaunchSpec::new(
        program(),
        vec!["graceful".into()],
        next_dir.path().into(),
        GracefulStop::MinecraftCommand,
    )
    .unwrap();
    let next = fixture.process.start(launch).unwrap();
    assert_ne!(stale, next);
    // Preserve directory lifetime through the owner's actual exit on every path.
    let mut replacement = Fixture {
        process: fixture.process,
        generation: next,
        console: fixture.console,
        dir: next_dir,
    };
    for result in [
        replacement.process.queue_line(stale, "leaked-command"),
        replacement.process.stop(stale, Duration::ZERO).map(|_| ()),
        replacement
            .process
            .force_kill(stale, Duration::ZERO)
            .map(|_| ()),
        replacement.process.wait(stale, Duration::ZERO).map(|_| ()),
    ] {
        assert_eq!(result, Err(RuntimeError::StaleGeneration));
    }
    replacement
        .process
        .queue_line(next, "replacement-alive")
        .unwrap();
    assert_eq!(replacement.recorded(1), ["replacement-alive"]);
    let snapshot = replacement.process.poll().unwrap();
    assert_eq!(snapshot.phase, ProcessPhase::Running);
    assert_eq!(snapshot.generation, Some(next));
    assert_eq!(snapshot.last_exit, Some(first));
    assert_eq!(replacement.stop().code, Some(0));
}

#[test]
fn unsupported_graceful_stop_is_exact_inert_refusal_and_kill_is_separate() {
    let mut fixture = fixture("graceful", GracefulStop::Unsupported);
    for _ in 0..2 {
        assert_eq!(
            fixture.process.stop(fixture.generation, Duration::ZERO),
            Err(RuntimeError::UnsupportedStop)
        );
        let snapshot = fixture.process.poll().unwrap();
        assert_eq!(snapshot.phase, ProcessPhase::Running);
        assert_eq!(snapshot.generation, Some(fixture.generation));
        assert_eq!(snapshot.last_exit, None);
    }
    fixture
        .process
        .queue_line(fixture.generation, "still-alive")
        .unwrap();
    assert_eq!(fixture.recorded(1), ["still-alive"]);
    wait_line(&fixture.console, ConsoleStream::Stdout, "echo:still-alive");
    let exit = exited(
        fixture
            .process
            .force_kill(fixture.generation, DEADLINE)
            .unwrap(),
    );
    assert!(!exit.success);
}

#[test]
fn both_pipes_drain_beyond_capacity_without_viewers() {
    let mut fixture = fixture("graceful", GracefulStop::MinecraftCommand);
    fixture
        .process
        .queue_line(fixture.generation, "flood")
        .unwrap();
    // No snapshot/read between flood admission and observed exit. The fixture cannot
    // read the stop until writing 4 MiB on EACH stream, far beyond pipe capacity.
    let exit = fixture.stop();
    assert_eq!(exit.code, Some(0));
    assert!(!exit.output_failed);
    assert_eq!(fixture.recorded(2), ["flood", "stop"]);
    let lines = all_lines(&fixture.console);
    for (stream, marker) in [
        (ConsoleStream::Stdout, "flood-stdout-done"),
        (ConsoleStream::Stderr, "flood-stderr-done"),
    ] {
        assert!(
            lines
                .iter()
                .any(|line| line.stream == stream && line.text == marker)
        );
    }
    let stdout: Vec<_> = lines
        .iter()
        .filter(|line| line.stream == ConsoleStream::Stdout)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(
        &stdout[stdout.len() - 2..],
        ["flood-stdout-done", "fixture-stopped"]
    );
}

#[test]
fn oversized_actual_pipe_line_is_bounded_and_following_line_survives() {
    let mut fixture = fixture("graceful", GracefulStop::MinecraftCommand);
    fixture
        .process
        .queue_line(fixture.generation, "long-line")
        .unwrap();
    assert_eq!(fixture.stop().code, Some(0));
    let lines = all_lines(&fixture.console);
    let long = lines.iter().find(|line| line.truncated).unwrap();
    assert_eq!(long.stream, ConsoleStream::Stdout);
    assert_eq!(long.text.len(), MAX_LINE_BYTES);
    assert!(long.text.bytes().all(|byte| byte == b'x'));
    assert!(
        lines
            .iter()
            .any(|line| line.cursor > long.cursor && line.text == "after-long-line")
    );
}

#[test]
fn wait_timeout_never_signals_and_nonreading_child_requires_explicit_kill() {
    let mut fixture = fixture("closed-stdin", GracefulStop::MinecraftCommand);
    let WaitOutcome::TimedOut(snapshot) = fixture
        .process
        .wait(fixture.generation, Duration::from_millis(20))
        .unwrap()
    else {
        panic!("nonreading fixture must stay alive");
    };
    assert_eq!(snapshot.phase, ProcessPhase::Running);
    let WaitOutcome::TimedOut(snapshot) = fixture
        .process
        .stop(fixture.generation, Duration::from_millis(20))
        .unwrap()
    else {
        panic!("unread stop cannot exit fixture");
    };
    assert_eq!(snapshot.phase, ProcessPhase::Stopping);
    let exit = exited(
        fixture
            .process
            .force_kill(fixture.generation, DEADLINE)
            .unwrap(),
    );
    assert!(!exit.success);
    assert_eq!(
        exited(
            fixture
                .process
                .wait(fixture.generation, Duration::ZERO)
                .unwrap()
        ),
        exit
    );
}

#[test]
fn excessive_timeouts_are_refused_before_effects() {
    let mut fixture = fixture("graceful", GracefulStop::MinecraftCommand);
    let invalid = MAX_WAIT + Duration::from_nanos(1);
    assert_eq!(
        fixture.process.stop(fixture.generation, invalid),
        Err(RuntimeError::InvalidTimeout)
    );
    assert_eq!(
        fixture.process.force_kill(fixture.generation, invalid),
        Err(RuntimeError::InvalidTimeout)
    );
    assert_eq!(
        fixture.process.wait(fixture.generation, invalid),
        Err(RuntimeError::InvalidTimeout)
    );
    assert_eq!(fixture.process.poll().unwrap().phase, ProcessPhase::Running);
    fixture
        .process
        .queue_line(fixture.generation, "unaffected")
        .unwrap();
    assert_eq!(fixture.recorded(1), ["unaffected"]);
    assert_eq!(fixture.stop().code, Some(0));
}
