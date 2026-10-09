//! Launch-only trusted TOML -> real owned inert native child; never Java/Minecraft.
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};
use vesta_game_runtime::{
    console::ConsoleCapture,
    launch_config::{LaunchConfigError, LaunchConfiguration},
    process::{GameProcess, ProcessPhase, RuntimeError, WaitOutcome},
};
const DEADLINE: Duration = Duration::from_secs(15);

fn document(working: &Path, launch: &str) -> Vec<u8> {
    format!(
        "[minecraft]\nworking-directory = '{}'\n[launch]\n{launch}\n",
        working.display()
    )
    .into_bytes()
}
fn program() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_vesta-game-fixture"))
}
fn recorded(cwd: &Path) -> Vec<String> {
    let bytes = fs::read(cwd.join("args.bin")).unwrap();
    let mut rest = bytes.as_slice();
    let mut args = Vec::new();
    while !rest.is_empty() {
        let (length, next) = rest.split_at(8);
        let length = usize::try_from(u64::from_le_bytes(length.try_into().unwrap())).unwrap();
        let (argument, next) = next.split_at(length);
        args.push(String::from_utf8(argument.to_vec()).unwrap());
        rest = next;
    }
    args
}
fn ready(console: &ConsoleCapture) {
    let until = Instant::now() + DEADLINE;
    loop {
        if console
            .snapshot(0, 100)
            .unwrap()
            .lines
            .iter()
            .any(|line| line.text == "fixture-ready")
        {
            return;
        }
        assert!(Instant::now() < until, "owned inert fixture not ready");
        thread::sleep(Duration::from_millis(5));
    }
}
fn exited(outcome: WaitOutcome) {
    let WaitOutcome::Exited(exit) = outcome else {
        panic!("owned fixture did not exit/join");
    };
    assert!(exit.success);
    assert!(!exit.output_failed && !exit.stdin_failed);
}
fn witness(launch: &str, expected: Vec<String>, selected_java: Option<&Path>) {
    let cwd = tempfile::tempdir().unwrap();
    fs::write(cwd.path().join("server.jar"), b"inert jar placeholder").unwrap();
    fs::write(
        cwd.path().join("first args.txt"),
        b"inert argfile, never interpreted",
    )
    .unwrap();
    fs::write(cwd.path().join("second.txt"), b"inert argfile").unwrap();
    let plan = LaunchConfiguration::parse_toml(&document(cwd.path(), launch), cwd.path()).unwrap();
    let spec = plan.into_launch_spec(selected_java).unwrap();
    let console = ConsoleCapture::new();
    // Process drops/joins before cwd even if an assertion panics.
    let mut process = GameProcess::new(console.clone());
    let generation = process.start(spec).unwrap();
    ready(&console);
    let expected = expected
        .into_iter()
        .map(|arg| arg.replace("$CWD", cwd.path().to_str().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(recorded(cwd.path()), expected);
    assert_eq!(
        Path::new(&fs::read_to_string(cwd.path().join("cwd.txt")).unwrap())
            .canonicalize()
            .unwrap(),
        cwd.path().canonicalize().unwrap()
    );
    assert!(!cwd.path().join("shell-effect").exists());
    exited(process.stop(generation, DEADLINE).unwrap());
    assert_eq!(
        fs::read_to_string(cwd.path().join("commands.txt")).unwrap(),
        "stop\n"
    );
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Stopped);
}

#[test]
fn native_configuration_preserves_exact_argv_cwd_and_shell_metacharacters() {
    witness(
        &format!(
            "kind = 'native'\nexecutable = '{}'\nstop = 'console-stop'\nargs = ['graceful', 'two words', '', '\"quote\"', '; echo secret > shell-effect', '🙂']",
            program().display()
        ),
        vec![
            "graceful".into(),
            "two words".into(),
            "".into(),
            "\"quote\"".into(),
            "; echo secret > shell-effect".into(),
            "🙂".into(),
        ],
        None,
    );
}

#[test]
fn jar_shape_is_witnessed_using_selected_inert_executable_not_real_java() {
    let path = Path::new("$CWD")
        .join("server.jar")
        .to_string_lossy()
        .into_owned();
    witness(
        "kind = 'java-jar'\njar = 'server.jar'\njvm-args = ['-Xmx1G', '-Dsecret=literal']\nargs = ['nogui', 'world name']",
        vec![
            "-Xmx1G".into(),
            "-Dsecret=literal".into(),
            "-jar".into(),
            path,
            "nogui".into(),
            "world name".into(),
        ],
        Some(program()),
    );
}

#[test]
fn ordered_argfile_shape_is_literal_and_never_reads_or_expands_argfile_contents() {
    let first = format!("@{}", Path::new("$CWD").join("first args.txt").display());
    let second = format!("@{}", Path::new("$CWD").join("second.txt").display());
    witness(
        &format!(
            "kind = 'java-argfile'\njava-command = '{}'\njvm-args = ['-Dflag=1']\nargfiles = ['first args.txt', 'second.txt']\nargs = ['nogui']",
            program().display()
        ),
        vec!["-Dflag=1".into(), first, second, "nogui".into()],
        None,
    );
}

#[test]
fn native_unsupported_stop_refuses_without_stdin_effect_and_retains_real_generation() {
    let cwd = tempfile::tempdir().unwrap();
    let plan = LaunchConfiguration::parse_toml(
        &document(
            cwd.path(),
            &format!(
                "kind = 'native'\nexecutable = '{}'\nstop = 'unsupported'",
                program().display()
            ),
        ),
        cwd.path(),
    )
    .unwrap();
    let console = ConsoleCapture::new();
    let mut process = GameProcess::new(console.clone());
    let generation = process.start(plan.into_launch_spec(None).unwrap()).unwrap();
    ready(&console);
    // The fixture's console readiness precedes opening its stdin-command log.
    // Wait for that actual file, rather than racing the child's next statement.
    let until = Instant::now() + DEADLINE;
    while !cwd.path().join("commands.txt").is_file() {
        assert!(
            Instant::now() < until,
            "owned fixture command log not created"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        process.stop(generation, Duration::ZERO),
        Err(RuntimeError::UnsupportedStop)
    );
    assert_eq!(process.poll().unwrap().generation, Some(generation));
    assert_eq!(fs::read(cwd.path().join("commands.txt")).unwrap(), b"");
    // Explicit fixture cleanup, not production automatic escalation.
    let WaitOutcome::Exited(_) = process.force_kill(generation, DEADLINE).unwrap() else {
        panic!("fixture cleanup");
    };
}

#[test]
fn artifacts_are_rechecked_at_real_spawn_before_effect_guard_or_process_creation() {
    let cwd = tempfile::tempdir().unwrap();
    let jar = cwd.path().join("server.jar");
    fs::write(&jar, b"inert").unwrap();
    let plan = LaunchConfiguration::parse_toml(
        &document(cwd.path(), "kind = 'java-jar'\njar = 'server.jar'"),
        cwd.path(),
    )
    .unwrap();
    let spec = plan.into_launch_spec(Some(program())).unwrap();
    fs::remove_file(jar).unwrap();
    let mut process = GameProcess::new(ConsoleCapture::new());
    let mut guarded = false;
    let result = process.start_guarded(&spec, |_| {
        guarded = true;
        Ok::<_, ()>(())
    });
    assert!(matches!(
        result,
        Err(vesta_game_runtime::process::GuardedError::Runtime(
            RuntimeError::InvalidLaunch
        ))
    ));
    assert!(!guarded);
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Stopped);
    assert!(!cwd.path().join("starts.txt").exists());
}

#[test]
fn missing_jar_or_argfile_and_script_executable_cannot_compile() {
    let cwd = tempfile::tempdir().unwrap();
    for launch in [
        "kind = 'java-jar'\njar = 'missing.jar'",
        "kind = 'java-argfile'\nargfiles = ['missing.txt']",
    ] {
        let plan =
            LaunchConfiguration::parse_toml(&document(cwd.path(), launch), cwd.path()).unwrap();
        assert_eq!(
            plan.into_launch_spec(Some(program())).unwrap_err(),
            LaunchConfigError::Preflight(RuntimeError::InvalidLaunch)
        );
    }
    let script = cwd.path().join("not-executable.CmD");
    fs::write(&script, b"inert").unwrap();
    let plan = LaunchConfiguration::parse_toml(
        &document(
            cwd.path(),
            &format!(
                "kind = 'native'\nexecutable = '{}'\nstop = 'unsupported'",
                script.display()
            ),
        ),
        cwd.path(),
    )
    .unwrap();
    assert!(plan.into_launch_spec(None).is_err());
    assert!(!cwd.path().join("starts.txt").exists());
}
