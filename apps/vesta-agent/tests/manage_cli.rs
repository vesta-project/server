//! Real short-binary and REPL IO, with registry-only fixtures and no live game.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};
const ID: &str = "srv_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("names")).unwrap();
    let instance = temp.path().join("instances").join(ID);
    fs::create_dir_all(&instance).unwrap();
    fs::write(instance.join("local-name"), b"owned").unwrap();
    fs::write(
        temp.path().join("names/owned.json"),
        format!(r#"{{"schema":1,"instance":"{ID}"}}"#),
    )
    .unwrap();
    temp
}
fn run(root: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vesta"))
        .args(args)
        .arg("--install-root")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn list_and_scoped_shell_never_create_state_or_start_a_server() {
    let root = fixture();
    let listed = run(root.path(), &["list", "--json"], "");
    assert!(listed.status.success());
    let data: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(data, serde_json::json!([{ "id": ID, "name": "owned" }]));
    for selector in [ID, "owned"] {
        for exit in ["exit", "quit", "back"] {
            let output = run(
                root.path(),
                &["manage", selector],
                &format!("help\n{exit}\nstart\n"),
            );
            assert!(output.status.success(), "{:?}", output.stderr);
            assert!(output.stderr.is_empty());
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("Server management")
            );
        }
        assert!(
            run(root.path(), &["manage", selector], "").status.success(),
            "EOF exits cleanly"
        );
        assert!(
            run(root.path(), &["manage", selector, "help"], "")
                .status
                .success()
        );
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    assert_eq!(
        fs::read_dir(root.path().join("instances").join(ID))
            .unwrap()
            .count(),
        1
    );
}
#[test]
fn scope_escape_and_bad_quoting_refuse_without_echoing_input_or_running_effects() {
    let root = fixture();
    for args in [
        vec!["manage", ID, "start", "fixture-secret"],
        vec!["manage", ID, "manage", "other"],
        vec!["manage", ID, "serve", "fixture-secret"],
    ] {
        let output = run(root.path(), &args, "");
        assert_eq!(output.status.code(), Some(2));
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("fixture-secret")
        );
    }
    let output = run(
        root.path(),
        &["manage", ID],
        "'fixture-secret\nhelp\nexit\n",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("fixture-secret")
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Server management")
    );
}
#[test]
fn aliases_cannot_redirect_a_management_session_to_another_instance() {
    let root = fixture();
    let other = "srv_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    fs::create_dir(root.path().join("instances").join(other)).unwrap();
    fs::write(
        root.path().join("instances").join(other).join("local-name"),
        b"owned",
    )
    .unwrap();
    let output = run(root.path(), &["manage", other, "help"], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("local_server_identity_changed")
    );
}
