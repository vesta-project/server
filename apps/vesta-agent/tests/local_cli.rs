//! Binary CLI checks only; no actual install/editor/game/listener execution.
use std::{fs, process::Command};
#[test]
fn versions_are_descriptive_and_update_refuses_any_binary_replacement() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("retained"), b"owned bytes").unwrap();
    let version = Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
        .current_dir(temp.path())
        .arg("version")
        .output()
        .unwrap();
    assert!(version.status.success());
    let text = String::from_utf8(version.stdout).unwrap();
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
    assert!(text.contains("signed_release_packages_supported"));
    let update = Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
        .current_dir(temp.path())
        .arg("update")
        .output()
        .unwrap();
    assert_eq!(update.status.code(), Some(1));
    assert!(
        String::from_utf8(update.stdout)
            .unwrap()
            .contains("available_version=unknown")
    );
    assert!(
        String::from_utf8(update.stderr)
            .unwrap()
            .contains("self_update_activation_not_implemented")
    );
    assert_eq!(
        fs::read(temp.path().join("retained")).unwrap(),
        b"owned bytes"
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}
#[test]
fn create_without_approval_never_probes_java_or_touches_an_existing_world() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("world")).unwrap();
    fs::write(temp.path().join("world/retained"), b"owned world fixture").unwrap();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
        .current_dir(temp.path())
        .args(["create", "owned", "--install-root"])
        .arg(temp.path())
        .args([
            "--minecraft-version",
            "1.21.1",
            "--java",
            "missing-inert-placeholder",
        ])
        .args(["--port", &port])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stderr,
        b"local_command_failed: local_install_approval_required_use_approve_install\n"
    );
    assert_eq!(
        fs::read(temp.path().join("world/retained")).unwrap(),
        b"owned world fixture"
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}
#[test]
fn strict_flags_and_confirmation_do_not_echo_rejected_secrets() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["create", "owned", "--accept-eula"],
        vec!["delete", "owned"],
        vec!["start", "owned", "--remote", "fixture-secret"],
        vec!["update", "--force"],
        vec!["config", "owned", "--shell", "fixture-secret"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
            .current_dir(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("fixture-secret")
        );
        assert!(output.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}
