//! Opt-in native sandbox acceptance with an inert fixture, never Minecraft.
use std::{
    fs,
    time::{Duration, Instant},
};
use vesta_game_runtime::{
    console::ConsoleCapture,
    isolation::ServerIsolation,
    process::{GameProcess, GracefulStop, LaunchSpec, RuntimeError, WaitOutcome},
};

#[test]
fn headless_native_permissions_stdio_exit_and_owner_identity() {
    if std::env::var_os("VESTA_RUN_NATIVE_SERVER_SANDBOX_TEST").is_none() {
        eprintln!(
            "native server sandbox test not requested; set VESTA_RUN_NATIVE_SERVER_SANDBOX_TEST=1 with the built shared helper"
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let runtime = root.path().join("runtime");
    let private = root.path().join("private");
    for path in [&game, &runtime, &private] {
        fs::create_dir(path).unwrap();
    }
    let program = runtime.join(if cfg!(windows) {
        "owned-fixture.exe"
    } else {
        "owned-fixture"
    });
    fs::copy(env!("CARGO_BIN_EXE_vesta-game-fixture"), &program).unwrap();
    let blocked = private.join("witness");
    let read_only = runtime.join("read-only");
    fs::write(&blocked, b"private-unchanged").unwrap();
    fs::write(&read_only, b"runtime-readable").unwrap();
    let make_spec = || {
        LaunchSpec::new(
            program.clone(),
            vec![
                "isolated-probe".into(),
                blocked.to_str().unwrap().into(),
                read_only.to_str().unwrap().into(),
            ],
            game.clone(),
            GracefulStop::MinecraftCommand,
        )
        .unwrap()
        .with_isolation(ServerIsolation::new(vec![runtime.clone()], vec![private.clone()]).unwrap())
        .unwrap()
    };
    let console = ConsoleCapture::new();
    let mut process = GameProcess::new(console.clone());
    let generation = process
        .start(make_spec())
        .expect("requested native isolation must be enforceable, never skipped/fallback");
    assert!(process.native_id().is_some());
    assert!(process.native_game_id().is_none());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !game.join("isolation-verified").exists() && Instant::now() < deadline {
        process.poll().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let verified = game.join("isolation-verified").exists();
    // Always request the fixture's clean stop before asserting output witnesses.
    let exit = process.stop(generation, Duration::from_secs(10)).unwrap();
    assert!(
        verified,
        "restricted fixture did not verify its permissions"
    );
    let WaitOutcome::Exited(exit) = exit else {
        panic!("sandbox failed to relay graceful stop")
    };
    assert!(exit.success && !exit.output_failed && !exit.stdin_failed);
    assert_eq!(fs::read(&blocked).unwrap(), b"private-unchanged");
    assert_eq!(fs::read(&read_only).unwrap(), b"runtime-readable");
    assert_eq!(
        fs::read_to_string(game.join("commands.txt")).unwrap(),
        "stop\n"
    );
    assert!(process.native_game_id().is_none());
    let denied = process.start_guarded(&make_spec(), |_| Err::<(), _>("fresh authority refusal"));
    assert!(matches!(
        denied,
        Err(vesta_game_runtime::process::GuardedError::Guard(
            "fresh authority refusal"
        ))
    ));
    assert_eq!(
        fs::read_to_string(game.join("starts.txt")).unwrap(),
        "start\n"
    );
    assert_ne!(
        RuntimeError::IsolationUnavailable.code(),
        RuntimeError::SpawnFailed.code()
    );
}
