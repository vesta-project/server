use std::{fs, process::Command};

const DOCUMENT: &str = r#"
schema-version = 1
[paths]
state = "private-state"
game = "game"
scan-roots = ["game/public"]
launch-definition = "launch.toml"
[pack]
name = "Owned fixture"
version = "1.0"
revision = 1
minimum-revision = 1
minecraft-version = "1.21.1"
[pack-access]
mode = "minecraft"
[listener]
mode = "disabled"
"#;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
}

#[test]
fn check_config_inspects_owned_configuration_without_creating_state_or_game_data() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("game/public")).unwrap();
    fs::write(temp.path().join("host.toml"), DOCUMENT).unwrap();
    fs::write(temp.path().join("game/retained"), b"owned fixture bytes").unwrap();
    let output = command()
        .current_dir(temp.path())
        .args(["check-config", "host.toml"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        output.stdout,
        b"configuration_valid; runtime_and_transport_not_started\n"
    );
    assert!(output.stderr.is_empty());
    assert!(!temp.path().join("private-state").exists());
    assert!(!temp.path().join("launch.toml").exists());
    assert!(!temp.path().join("game/eula.txt").exists());
    assert_eq!(
        fs::read(temp.path().join("game/retained")).unwrap(),
        b"owned fixture bytes"
    );
}

#[test]
fn malformed_config_and_native_errors_never_echo_input_or_paths() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("secret-path.toml"),
        b"owner-secret = 'fixture-secret'",
    )
    .unwrap();
    for (source, code) in [
        ("secret-path.toml", "host_config_invalid"),
        ("missing-secret-path.toml", "host_config_missing"),
    ] {
        let output = command()
            .current_dir(temp.path())
            .args(["check-config", source])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(error, format!("configuration_failed: {code}\n"));
        assert!(!error.contains("secret"));
        assert!(!error.contains(&temp.path().to_string_lossy().to_string()));
        assert!(!temp.path().join("private-state").exists());
    }
}

#[test]
fn state_overlap_with_either_visible_root_refuses_without_preparing_any_state() {
    for game_or_scan in ["game/private-state", "public/private-state"] {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("game")).unwrap();
        fs::create_dir_all(temp.path().join("public")).unwrap();
        let document = DOCUMENT
            .replace(
                "state = \"private-state\"",
                &format!("state = \"{game_or_scan}\""),
            )
            .replace(
                "scan-roots = [\"game/public\"]",
                "scan-roots = [\"public\"]",
            );
        fs::write(temp.path().join("host.toml"), document).unwrap();
        let output = command()
            .current_dir(temp.path())
            .args(["check-config", "host.toml"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!temp.path().join(game_or_scan).exists());
    }
}

#[test]
fn absolute_source_keeps_relative_host_paths_anchored_to_explicit_cwd() {
    let base = tempfile::tempdir().unwrap();
    let source_home = tempfile::tempdir().unwrap();
    fs::create_dir_all(base.path().join("game/public")).unwrap();
    fs::write(base.path().join("game/retained"), b"owned").unwrap();
    let game = base
        .path()
        .join("game")
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let document = DOCUMENT
        .replace(
            "state = \"private-state\"",
            "state = \"game/private-state\"",
        )
        .replace("game = \"game\"", &format!("game = \"{game}\""))
        .replace(
            "scan-roots = [\"game/public\"]",
            &format!("scan-roots = [\"{game}/public\"]"),
        );
    let source = source_home.path().join("host.toml");
    fs::write(&source, document).unwrap();
    let output = command()
        .current_dir(base.path())
        .arg("check-config")
        .arg(&source)
        .output()
        .unwrap();
    // Resolving state against the source's directory instead would miss this overlap.
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"configuration_failed: host_config_state_overlap\n"
    );
    assert!(!base.path().join("game/private-state").exists());
    assert!(!source_home.path().join("game").exists());
    assert_eq!(
        fs::read(base.path().join("game/retained")).unwrap(),
        b"owned"
    );
}

#[test]
fn unsupported_host_commands_and_extra_arguments_do_not_load_configuration() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["run"],
        vec!["bootstrap-owner"],
        vec!["check-config"],
        vec!["check-config", "missing.toml", "extra"],
    ] {
        let output = command()
            .current_dir(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.starts_with(b"Vesta agent\n"));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    let output = command().arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(output.stdout.starts_with(b"Vesta agent\n"));
}
