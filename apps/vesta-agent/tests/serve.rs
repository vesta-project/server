//! Trusted-local CLI refusal tests; no live listener or game process is started.
use std::{fs, process::Command, sync::Arc};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

const DOCUMENT: &str = "schema-version = 1\n[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['game/config']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'Owned fixture'\nversion = '1.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n";

#[test]
fn serve_refuses_disabled_listener_without_opening_state_or_launching() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("game/config")).unwrap();
    fs::write(temp.path().join("host.toml"), DOCUMENT).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
        .current_dir(temp.path())
        .args(["serve", "host.toml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"listener_disabled\n");
    assert!(!temp.path().join("private-state").exists());
    assert!(!temp.path().join("private/launch.toml").exists());
}

#[test]
fn serve_failed_tls_start_closes_the_host_and_never_generates_material() {
    let temp = tempfile::tempdir().unwrap();
    for dir in ["game/config", "private", "bin"] {
        fs::create_dir_all(temp.path().join(dir)).unwrap();
    }
    // Launch preflight names an inert placeholder; Start is never admitted.
    fs::write(temp.path().join("bin/inert-placeholder"), b"").unwrap();
    fs::write(temp.path().join("private/launch.toml"), "[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/inert-placeholder'\nstop = 'console-stop'\n").unwrap();
    // TLS material is deliberately absent: refusal occurs before any bind attempt.
    let document = DOCUMENT.replace("mode = 'disabled'", "mode = 'tls'\nbind = '127.0.0.1'\nport = 25567\ncertificate = 'private/test-only-missing-cert.pem'\nprivate-key = 'private/test-only-missing-key.pem'\nmax-connections = 1\nmax-request-bytes = 1024\nrequest-timeout-seconds = 1");
    fs::write(temp.path().join("host.toml"), document).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
        .current_dir(temp.path())
        .args(["serve", "host.toml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    // Local commit 7a7f49a made listener refusal diagnostics specific and fixed-code.
    // Preserve the strict complete stderr assertion and the cleanup checks below.
    assert_eq!(output.stderr, b"listener_failed: tls_material_unavailable\n");
    assert!(
        !temp
            .path()
            .join("private/test-only-missing-cert.pem")
            .exists()
    );
    assert!(
        !temp
            .path()
            .join("private/test-only-missing-key.pem")
            .exists()
    );
    let directory = StateDirectory::new(
        temp.path().join("private-state"),
        Some(&temp.path().join("game")),
    )
    .unwrap();
    let db = StateDatabase::open(directory, Arc::new(SystemClock)).unwrap();
    db.close().unwrap();
}
