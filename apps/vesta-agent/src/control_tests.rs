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
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(line) if line == "stop" => std::process::exit(0),
            Ok(line) if line == "list" => {
                use std::io::Write;
                println!("There are 1 of a max of 20 players online: OwnedAlpha");
                std::io::stdout().flush().unwrap();
            }
            _ => {}
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
            vesta_control::identity::management::ManagementAccess::new(db.clone()).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
        let status = control::request_data(db.directory().path(), "owned".into(), Op::Status)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status["agent"], "online");
        assert_eq!(status["state"], "stopped");
        let players = control::request_data(db.directory().path(), "owned".into(), Op::PlayersList)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(players["available"], false);
        assert!(players["players"].is_null());
        assert_eq!(
            control::request_data(db.directory().path(), "other".into(), Op::PlayersList).await,
            Err("local_control_request_refused")
        );
        assert_eq!(
            control::request(db.directory().path(), "owned".into(), Op::Start).await,
            Err("local_eula_pending_owner_must_accept_themselves")
        );

        // Identity commands do not require or accept a game EULA. Native IPC
        // proves the OS account before the local owner seam can issue authority.
        let paired = control::request_data(
            db.directory().path(),
            "owned".into(),
            Op::Pair {
                role: "admin".into(),
                minutes: 10,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(paired["role"], "administrator");
        assert!(paired["tls_sha256_fingerprint"].is_null());
        assert!(paired["pairing_link"].is_null());
        assert_eq!(paired["code"].as_str().unwrap().len(), 43);
        assert_eq!(
            control::request_data(
                db.directory().path(),
                "wrong-name".into(),
                Op::Pair {
                    role: "owner".into(),
                    minutes: 10
                }
            )
            .await,
            Err("local_control_request_refused")
        );
        assert_eq!(
            control::request_data(
                db.directory().path(),
                "owned".into(),
                Op::Pair {
                    role: "owner".into(),
                    minutes: 61
                }
            )
            .await,
            Err("local_control_request_refused")
        );
        let sources = control::request_data(db.directory().path(), "owned".into(), Op::AccessShow)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sources["anonymous"], "public");
        assert_eq!(
            control::request_data(
                db.directory().path(),
                "owned".into(),
                Op::AccessSet {
                    fields: vec![("anonymous".into(), "owner".into())]
                }
            )
            .await,
            Err("local_control_request_refused")
        );
        let changed = control::request_data(
            db.directory().path(),
            "owned".into(),
            Op::AccessSet {
                fields: vec![("whitelist_enabled".into(), "off".into())],
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(changed["whitelist_enabled"], false);
        db.read(|c| {
            let count: u32 = c.query_row(
                "SELECT count(*) FROM audit_events WHERE action='access.sources.set'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
        let identity =
            vesta_control::identity::management::ManagementAccess::new(db.clone()).unwrap();
        let enrolled = identity
            .enroll(
                paired["code"].as_str().unwrap(),
                "IPC device",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )
            .unwrap();
        let listed = control::request_data(db.directory().path(), "owned".into(), Op::DevicesList)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(listed[0]["device_id"], enrolled.device_id);
        control::request_data(
            db.directory().path(),
            "owned".into(),
            Op::DevicesRevoke {
                device: enrolled.device_id,
            },
        )
        .await
        .unwrap();
        assert!(identity.list_local().unwrap()[0].revoked);
        let uuid = "11111111-2222-4333-8444-555555555555".to_owned();
        let linked = control::request_data(
            db.directory().path(),
            "owned".into(),
            Op::OwnerLink {
                uuid: uuid.clone(),
                role: "owner".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(linked["linked"], true);
        assert!(linked["tls_sha256_fingerprint"].is_null()); // This fixture is local-only.
        let links = control::request_data(db.directory().path(), "owned".into(), Op::OwnerList)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(links.as_array().unwrap().len(), 1);
        control::request_data(
            db.directory().path(),
            "owned".into(),
            Op::OwnerUnlink {
                uuid,
                revoke_devices: true,
            },
        )
        .await
        .unwrap();
        assert!(identity.links_local().unwrap().is_empty());
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
        let listed = control::request_data(db.directory().path(), "owned".into(), Op::PlayersList)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(listed["available"], true);
        assert_eq!(listed["players"], serde_json::json!(["OwnedAlpha"]));
        assert_eq!(listed["online"], 1);
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
        let shutdown = server.shutdown_signal();
        assert!(!*shutdown.borrow());
        control::request(db.directory().path(), "owned".into(), Op::Shutdown)
            .await
            .unwrap();
        assert!(*shutdown.borrow());
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
