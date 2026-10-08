use super::*;
use std::{fs, path::PathBuf};
use tempfile::TempDir;

fn spec(args: Vec<String>, cwd: &TempDir) -> Result<LaunchSpec, RuntimeError> {
    LaunchSpec::new(
        std::env::current_exe().unwrap(),
        args,
        cwd.path().to_path_buf(),
        GracefulStop::MinecraftCommand,
    )
}

#[test]
fn argument_count_and_individual_byte_limits_are_inclusive() {
    let cwd = TempDir::new().unwrap();
    assert!(spec(vec![String::new(); MAX_ARGS], &cwd).is_ok());
    assert!(matches!(
        spec(vec![String::new(); MAX_ARGS + 1], &cwd),
        Err(RuntimeError::InvalidLaunch)
    ));
    assert!(spec(vec!["x".repeat(MAX_ARG_BYTES)], &cwd).is_ok());
    assert!(matches!(
        spec(vec!["x".repeat(MAX_ARG_BYTES + 1)], &cwd),
        Err(RuntimeError::InvalidLaunch)
    ));
    assert!(spec(vec!["€".repeat(MAX_ARG_BYTES / 3)], &cwd).is_ok());
    assert!(matches!(
        spec(vec!["€".repeat(MAX_ARG_BYTES / 3 + 1)], &cwd),
        Err(RuntimeError::InvalidLaunch)
    ));
}

#[test]
fn total_utf8_budget_has_a_real_exact_boundary_independent_of_native_units() {
    let cwd = TempDir::new().unwrap();
    let program = std::env::current_exe().unwrap();
    let overhead = program.to_str().unwrap().len() + 1 + 4;
    let mut remaining = MAX_ARGV_BYTES - overhead;
    let mut args = Vec::new();
    for _ in 0..4 {
        let bytes = remaining.min(MAX_ARG_BYTES);
        args.push("€".repeat(bytes / 3) + &"x".repeat(bytes % 3));
        remaining -= bytes;
    }
    assert_eq!(remaining, 0);
    assert!(spec(args.clone(), &cwd).is_ok());
    args[3].push('x');
    assert!(matches!(spec(args, &cwd), Err(RuntimeError::InvalidLaunch)));
}

#[test]
fn native_quoting_expansion_cannot_hide_behind_utf8_budget() {
    let cwd = TempDir::new().unwrap();
    // Four trailing-backslash arguments double in conservative native quoting.
    assert!(matches!(
        spec(vec!["\\".repeat(4096); 4], &cwd),
        Err(RuntimeError::InvalidLaunch)
    ));
    assert!(spec(vec!["x".repeat(4096); 4], &cwd).is_ok());
}

#[test]
fn controls_and_line_separators_are_refused_without_echo() {
    let cwd = TempDir::new().unwrap();
    for c in [
        '\0', '\n', '\r', '\t', '\u{7f}', '\u{85}', '\u{2028}', '\u{2029}',
    ] {
        let input = format!("secret-launch-39c8{c}");
        let error = spec(vec![input], &cwd).unwrap_err();
        assert_eq!(error, RuntimeError::InvalidLaunch);
        assert!(!format!("{error:?} {error}").contains("secret-launch-39c8"));
        assert!(!valid_command(&format!("secret-command-819d{c}")));
    }
}

#[test]
fn command_byte_blank_and_timeout_bounds_are_exact() {
    assert!(valid_command(&"x".repeat(MAX_COMMAND_BYTES)));
    assert!(!valid_command(&"x".repeat(MAX_COMMAND_BYTES + 1)));
    assert!(valid_command(&"€".repeat(MAX_COMMAND_BYTES / 3)));
    assert!(!valid_command(&"€".repeat(MAX_COMMAND_BYTES / 3 + 1)));
    for line in ["", " ", "\u{2003}", "hello\nstop", "hello\rstop"] {
        assert!(!valid_command(line));
    }
    assert!(deadline(Duration::ZERO).is_ok());
    assert!(deadline(MAX_WAIT).is_ok());
    assert_eq!(
        deadline(MAX_WAIT + Duration::from_nanos(1)),
        Err(RuntimeError::InvalidTimeout)
    );
}

#[test]
fn paths_are_absolute_typed_and_redacted_and_scripts_never_launch() {
    let cwd = TempDir::new().unwrap();
    let program = cwd.path().join("launch-path-secret-983e.exe");
    fs::write(&program, b"not executable").unwrap();
    let spec = LaunchSpec::new(
        program.clone(),
        vec!["argv-secret-a613".into()],
        cwd.path().into(),
        GracefulStop::Unsupported,
    )
    .unwrap();
    let debug = format!("{spec:?}");
    assert!(!debug.contains("launch-path-secret-983e"));
    assert!(!debug.contains("argv-secret-a613"));
    assert!(matches!(
        LaunchSpec::new(
            PathBuf::from("relative.exe"),
            vec![],
            cwd.path().into(),
            GracefulStop::Unsupported
        ),
        Err(RuntimeError::InvalidLaunch)
    ));
    assert!(matches!(
        LaunchSpec::new(
            program,
            vec![],
            PathBuf::from("relative"),
            GracefulStop::Unsupported
        ),
        Err(RuntimeError::InvalidLaunch)
    ));
    for extension in ["cmd", "BAT"] {
        let script = cwd.path().join(format!("secret-script.{extension}"));
        fs::write(&script, b"never execute").unwrap();
        assert!(matches!(
            LaunchSpec::new(script, vec![], cwd.path().into(), GracefulStop::Unsupported),
            Err(RuntimeError::InvalidLaunch)
        ));
    }
}

#[test]
fn failed_spawn_is_redacted_and_does_not_publish_a_generation() {
    let cwd = TempDir::new().unwrap();
    let executable = cwd.path().join("invalid-native-secret-97c3.exe");
    fs::write(&executable, b"not an executable").unwrap();
    let launch = LaunchSpec::new(
        executable,
        vec![],
        cwd.path().into(),
        GracefulStop::Unsupported,
    )
    .unwrap();
    let mut process = GameProcess::new(ConsoleCapture::new());
    let error = process.start(launch).unwrap_err();
    assert_eq!(error, RuntimeError::SpawnFailed);
    assert!(!format!("{error} {error:?} {process:?}").contains("secret-97c3"));
    let snapshot = process.poll().unwrap();
    assert_eq!(snapshot.phase, ProcessPhase::Stopped);
    assert_eq!(snapshot.generation, None);
    assert_eq!(snapshot.last_exit, None);
}

#[test]
fn preflight_rechecks_directory_before_any_process_is_created() {
    let cwd = TempDir::new().unwrap();
    let subdir = cwd.path().join("removed-cwd");
    fs::create_dir(&subdir).unwrap();
    let launch = LaunchSpec::new(
        std::env::current_exe().unwrap(),
        vec![],
        subdir.clone(),
        GracefulStop::Unsupported,
    )
    .unwrap();
    fs::remove_dir(subdir).unwrap();
    let mut process = GameProcess::new(ConsoleCapture::new());
    assert_eq!(process.start(launch), Err(RuntimeError::InvalidLaunch));
    assert_eq!(process.poll().unwrap().phase, ProcessPhase::Stopped);
}
