//! Opt-in integration against staged real Vesta commands and an owned inert game.
use super::*;
use std::io::{BufRead, Write};

const ID: &str = "srv_0123456789abcdef0123456789abcdef";

#[test]
fn inert_game() {
    if fs::read("owned-inert-game").ok().as_deref() != Some(b"vesta-dev-restart-fixture") {
        return;
    }
    println!("owned inert process ready");
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        if line.unwrap() == "stop" {
            return;
        }
    }
}

#[test]
#[ignore = "requires VESTA_DEV_TEST_BIN with a staged complete native build; uses owned inert processes only"]
fn real_restart_preserves_running_and_stopped_games() {
    let bin = PathBuf::from(std::env::var_os("VESTA_DEV_TEST_BIN").expect("VESTA_DEV_TEST_BIN"));
    for separate in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("registry");
        let directory = root.join("instances").join(ID);
        fs::create_dir_all(directory.join("game/config")).unwrap();
        fs::create_dir_all(root.join("names")).unwrap();
        fs::write(
            root.join("names/example.json"),
            format!("{{\"schema\":1,\"instance\":\"{ID}\"}}"),
        )
        .unwrap();
        fs::write(directory.join("local-name"), "example").unwrap();
        let source = directory.join("host.toml");
        let literal = |path: &Path| {
            serde_json::to_string(&path.to_string_lossy().replace('\\', "/")).unwrap()
        };
        let document = format!(
            "schema-version=1\n[paths]\nstate={}\ngame={}\nscan-roots=[{}]\nlaunch-definition={}\n[pack]\nname='Owned inert fixture'\nversion='1'\nrevision=1\nminimum-revision=1\nminecraft-version='1.21.1'\n[pack-access]\nmode='minecraft'\n[listener]\nmode='disabled'\n[process]\nsupervision='{}'\n",
            literal(&directory.join("state")),
            literal(&directory.join("game")),
            literal(&directory.join("game/config")),
            literal(&directory.join("launch.toml")),
            if separate { "separate" } else { "in-process" }
        );
        fs::write(&source, document).unwrap();
        let fixture = directory.join(executable("inert-game"));
        fs::copy(std::env::current_exe().unwrap(), &fixture).unwrap();
        fs::write(directory.join("launch.toml"), format!("[minecraft]\nworking-directory={}\n[launch]\nkind='native'\nexecutable={}\nstop='console-stop'\nargs=['--exact','restart::live_tests::inert_game','--nocapture']\n", literal(&directory.join("game")), literal(&fixture))).unwrap();
        fs::write(
            directory.join("game/owned-inert-game"),
            b"vesta-dev-restart-fixture",
        )
        .unwrap();
        // Consent applies only to this owned inert executable, never to Minecraft.
        fs::write(
            directory.join("game/eula.txt"),
            "# Owned inert fixture only\neula=true\n",
        )
        .unwrap();
        let agent = Agent {
            binary: bin.join(executable("vesta")),
            root: root.clone(),
            id: ID.into(),
            base: directory.clone(),
        };
        let mut original =
            spawn(&agent, &source, true, &temp.path().join("original-logs")).unwrap();
        let exercise = (|| -> Result<()> {
            let deadline = Instant::now() + WAIT;
            loop {
                if agent.manage("status").is_ok() {
                    break;
                }
                if original.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                    return Err("original fixture failed to start".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let config = HostConfiguration::load_file(&source, &directory).unwrap();
            for running in [true, false] {
                if running {
                    agent.manage("start")?;
                }
                let before = agent.manage("status")?;
                assert_eq!(was_running(&before)?, running);
                run(&bin, temp.path(), &root, ID, None)?;
                let after = agent.manage("status")?;
                assert_ne!(before["agentPid"], after["agentPid"]);
                assert_eq!(after["localOnly"], true);
                assert_eq!(was_running(&after)?, running);
                if running {
                    assert_ne!(before["generation"], after["generation"]);
                    agent.manage("stop")?;
                }
            }
            agent
                .command()
                .args(["agent", "shutdown"])
                .arg(&source)
                .status()
                .map_err(|e| e.to_string())?;
            let until = Instant::now() + WAIT;
            while !quiescent(&source, &config)? {
                if Instant::now() >= until {
                    return Err("fixture cleanup timed out".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        })();
        if let Err(error) = exercise {
            // Do not erase data belonging to any potentially surviving fixture.
            let preserved = temp.keep();
            panic!(
                "{error}; preserved owned fixture at {}",
                preserved.display()
            );
        }
        original.wait().unwrap();
    }
}
