//! Real protected local IPC, owned temporary state and inert copied test children.
//! No Java, Minecraft, shell, public listener, service or real EULA acceptance.
use crate::control::{self, Op};
use std::{fs, io::BufRead, path::PathBuf, sync::Arc, time::Duration};
use vesta_control::operations::{OperationRuntime, Operations, ShutdownOutcome};
use vesta_game_runtime::process::{GracefulStop, LaunchSpec, ProcessPhase};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};
const MARKER: &[u8] = b"owned-inert-local-cli-fixture-v1";
#[test]
fn inert_fixture() {
    if fs::read("inert-fixture").ok().as_deref() != Some(MARKER) {
        return;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if let Ok(line) = line {
                if sender.send(line).is_err() {
                    return;
                }
            } else {
                return;
            }
        }
    });
    loop {
        if fs::read("fixture-release").ok().as_deref() == Some(b"release") {
            std::process::exit(0);
        }
        if receiver
            .recv_timeout(Duration::from_millis(5))
            .is_ok_and(|s| s == "stop")
        {
            std::process::exit(0);
        }
    }
}
struct Owned {
    runtime: OperationRuntime,
    game: PathBuf,
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = fs::write(self.game.join("fixture-release"), b"release");
        let _ = self.runtime.shutdown(Duration::from_secs(15));
    }
}
#[test]
fn protected_channel_start_stop_restart_pending_eula_and_absent_agent() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("inert-game");
    fs::create_dir(&game).unwrap();
    fs::write(game.join("inert-fixture"), MARKER).unwrap();
    let executable = game.join(if cfg!(windows) { "inert.exe" } else { "inert" });
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let directory = StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
    let db = StateDatabase::open(directory, Arc::new(SystemClock)).unwrap();
    let launch = LaunchSpec::new(
        executable,
        vec![
            "--exact".into(),
            "control_tests::inert_fixture".into(),
            "--nocapture".into(),
        ],
        game.clone(),
        GracefulStop::MinecraftCommand,
    )
    .unwrap();
    let mut owned = Owned {
        runtime: OperationRuntime::open(Operations::new(db.clone()).unwrap(), launch).unwrap(),
        game: game.clone(),
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        assert_eq!(
            control::request(db.directory().path(), "owned".into(), Op::Start).await,
            Err("local_agent_not_running_start_serve_first")
        );
        let server = control::Server::start(
            db.directory().path(),
            "owned".into(),
            game.clone(),
            owned.runtime.local_handle(),
        )
        .await
        .unwrap();
        assert_eq!(
            control::request(db.directory().path(), "owned".into(), Op::Start).await,
            Err("local_eula_pending_owner_must_accept_themselves")
        );
        // Synthetic owner-accepted configuration for this inert test child only.
        // This is not a real install and does not accept a Minecraft EULA.
        fs::write(
            game.join("eula.txt"),
            b"# Inert test fixture only\neula=true\n",
        )
        .unwrap();
        control::request(db.directory().path(), "owned".into(), Op::Start)
            .await
            .unwrap();
        let first = owned.runtime.game_snapshot().unwrap().generation;
        assert!(first.is_some());
        assert!(
            control::request(db.directory().path(), "other".into(), Op::Stop)
                .await
                .is_err()
        );
        control::request(db.directory().path(), "owned".into(), Op::Restart)
            .await
            .unwrap();
        assert_ne!(first, owned.runtime.game_snapshot().unwrap().generation);
        control::request(db.directory().path(), "owned".into(), Op::Stop)
            .await
            .unwrap();
        assert_eq!(
            owned.runtime.game_snapshot().unwrap().phase,
            ProcessPhase::Stopped
        );
        server.shutdown().await.unwrap();
        assert_eq!(
            control::request(db.directory().path(), "owned".into(), Op::Stop).await,
            Err("local_agent_not_running_start_serve_first")
        );
    });
    assert_eq!(
        owned.runtime.shutdown(Duration::from_secs(15)).unwrap(),
        ShutdownOutcome::Complete
    );
    drop(owned);
    db.close().unwrap();
}
