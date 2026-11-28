//! Disposable host binary signal smoke test. Never uses an installed host or profile.
#![cfg(unix)]
use std::{
    fs,
    io::Read,
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
#[test]
fn host_binary_drains_cleanly_on_sigterm_and_sigint() {
    for signal in ["TERM", "INT"] {
        let temp = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/vesta-transport/tests/fixtures");
        fs::copy(
            fixtures.join("test-only-server.pem"),
            temp.path().join("certificate.pem"),
        )
        .unwrap();
        fs::copy(
            fixtures.join("test-only-server-key.pem"),
            temp.path().join("key.pem"),
        )
        .unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(temp.path().join("host.toml"),format!("state='host-state'\ncertificate='certificate.pem'\nprivate-key='key.pem'\nbind='127.0.0.1'\nport={port}\nlocal-only=true\n")).unwrap();
        let mut child = OwnedChild(
            Command::new(env!("CARGO_BIN_EXE_vesta-agent"))
                .arg("host")
                .arg(temp.path().join("host.toml"))
                .current_dir(temp.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let ready = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if let Some(status) = child.0.try_wait().unwrap() {
                let mut errors = String::new();
                child
                    .0
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut errors)
                    .unwrap();
                panic!("host exited before ready: {status} {errors}");
            }
            assert!(Instant::now() < ready, "disposable host did not bind");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(child.0.id().to_string())
                .status()
                .unwrap()
                .success()
        );
        let until = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < until,
                "host failed to drain after {signal}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut errors = String::new();
        child
            .0
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut errors)
            .unwrap();
        assert!(status.success(), "{signal}: {errors}");
        assert!(errors.contains("host_shutdown_complete"), "{errors}");
        let state = vesta_control::machine::HostState::open(
            temp.path().join("host-state"),
            std::sync::Arc::new(vesta_state::SystemClock),
        )
        .unwrap();
        assert!(state.snapshot().unwrap().primary_account_id.is_none());
        state.database().close().unwrap();
    }
}
use std::path::Path;
