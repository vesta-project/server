//! Literal native argv, no-effect validation and bounded stdin backpressure.
mod common;
use common::*;
use std::{
    fs,
    time::{Duration, Instant},
};
use vesta_game_runtime::process::{GracefulStop, MAX_COMMAND_BYTES, ProcessPhase, RuntimeError};

#[test]
fn argv_is_literal_including_shell_syntax_quotes_empty_unicode_and_argfile_tokens() {
    let args = vec![
        "graceful",
        "",
        "with spaces",
        "trailing\\",
        "quote\"slash\\\"",
        "🙂€",
        "%COMSPEC%",
        "$(touch shell-side-effect)",
        "; touch shell-side-effect",
        "> shell-side-effect",
        "-jar",
        "literal.jar",
        "@literal-args.txt",
        "argv-secret-b7a2",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let mut fixture = Fixture::new(args.clone(), GracefulStop::MinecraftCommand);
    let bytes = fs::read(fixture.dir.path().join("args.bin")).unwrap();
    let mut offset = 0;
    let mut observed = Vec::new();
    while offset < bytes.len() {
        let length = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        observed.push(
            std::str::from_utf8(&bytes[offset..offset + length])
                .unwrap()
                .to_owned(),
        );
        offset += length;
    }
    assert_eq!(observed, args);
    assert!(!fixture.dir.path().join("shell-side-effect").exists());
    assert!(!format!("{:?}", fixture.process).contains("argv-secret-b7a2"));
    assert_eq!(fixture.stop().code, Some(0));
}

#[test]
fn invalid_console_lines_never_reach_child_and_exact_byte_boundary_does() {
    let mut fixture = Fixture::new(vec!["graceful".into()], GracefulStop::MinecraftCommand);
    for line in [
        "",
        " ",
        "secret-console-9c72\nstop",
        "secret-console-9c72\rstop",
        "secret-console-9c72\0",
        "secret-console-9c72\u{85}",
        "secret-console-9c72\u{2028}",
        "secret-console-9c72\u{2029}",
    ] {
        let error = fixture
            .process
            .queue_line(fixture.generation, line)
            .unwrap_err();
        assert_eq!(error, RuntimeError::InvalidCommand);
        assert!(!format!("{error:?} {error}").contains("secret-console-9c72"));
    }
    assert_eq!(
        fixture
            .process
            .queue_line(fixture.generation, &"x".repeat(MAX_COMMAND_BYTES + 1)),
        Err(RuntimeError::InvalidCommand)
    );
    let exact = "x".repeat(MAX_COMMAND_BYTES);
    fixture
        .process
        .queue_line(fixture.generation, &exact)
        .unwrap();
    assert_eq!(fixture.recorded(1), [exact]);
    assert_eq!(fixture.stop().code, Some(0));
    // Fully observed exit plus recorded ordered commands is the no-injection witness.
    assert_eq!(fixture.recorded(2).last().unwrap(), "stop");
    assert_eq!(fixture.recorded(2).len(), 2);
}

#[test]
fn nonreading_stdin_has_bounded_queue_and_pending_writes_fail_after_kill() {
    let mut fixture = Fixture::new(vec!["closed-stdin".into()], GracefulStop::MinecraftCommand);
    let payload = "x".repeat(MAX_COMMAND_BYTES);
    let started = Instant::now();
    let mut refused = false;
    for _ in 0..1024 {
        match fixture.process.queue_line(fixture.generation, &payload) {
            Ok(()) => (),
            Err(RuntimeError::StdinBusy) => {
                refused = true;
                break;
            }
            Err(error) => panic!("unexpected stdin error: {error:?}"),
        }
    }
    assert!(
        refused,
        "nonreading fixture must exert bounded backpressure"
    );
    assert!(
        started.elapsed() < DEADLINE,
        "queue admission must not wait for pipe writes"
    );
    assert_eq!(fixture.process.poll().unwrap().phase, ProcessPhase::Running);
    let exit = exited(
        fixture
            .process
            .force_kill(fixture.generation, DEADLINE)
            .unwrap(),
    );
    assert!(!exit.success);
    // Pending native writes fail after kill. Queue-full alone does not prove the
    // writer had already entered a blocked write before the kill.
    assert!(exit.stdin_failed);
    assert_eq!(fixture.process.poll().unwrap().phase, ProcessPhase::Stopped);
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
