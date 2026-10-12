//! A workflow drives a real backup job through the host action port, as its execution
//! principal's automation device, and loses that authority when the device is revoked.
use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};
use vesta_control::{
    automation::{Automation, WorkflowDefinition},
    automation_host::{AUTOMATION_DEVICE_LABEL, HostActions},
    backups::{Backups, Environment},
    config::{BackupMode, BackupProfile, BackupSettings, MutationPolicy},
    files::FileBrowser,
    identity::{DeviceId, PrincipalId, Subject},
    operations::{OperationRuntime, Operations},
};
use vesta_game_runtime::process::{GracefulStop, LaunchSpec};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn a_manual_workflow_runs_a_backup_and_revocation_suspends_it() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    fs::create_dir_all(game.join("world")).unwrap();
    fs::write(game.join("world/level.dat"), b"level").unwrap();
    let state = StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
    let db = StateDatabase::open(state, Arc::new(SystemClock)).unwrap();
    let owner = Subject {
        principal: PrincipalId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap(),
        device: DeviceId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap(),
    };
    db.write(|c| {
        let t = db.clock().millis();
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Owner','active',?,?)", rusqlite::params![owner.principal.as_str(), db.server_id(), t, t])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Laptop',?,?)", rusqlite::params![owner.device.as_str(), db.server_id(), owner.principal.as_str(), "1".repeat(64), t])?;
        c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('cccccccc-cccc-4ccc-8ccc-cccccccccccc',?,'owner','fixture',?)", rusqlite::params![owner.principal.as_str(), t])?;
        Ok(())
    })
    .unwrap();
    // A runtime that never launches anything in this test.
    let launch = LaunchSpec::new(
        std::env::current_exe().unwrap(),
        vec![],
        game.clone(),
        GracefulStop::MinecraftCommand,
    )
    .unwrap();
    let mut runtime = OperationRuntime::open(Operations::new(db.clone()).unwrap(), launch).unwrap();
    let handle = runtime.network_handle();
    let cell: Arc<std::sync::OnceLock<vesta_control::operations::RuntimeHandle>> = Arc::default();
    let _ = cell.set(handle.clone());
    let settings = BackupSettings {
        repository: vesta_control::config::BackupRepository::Local(temp.path().join("repo")),
        restore_root: None,
        profiles: vec![BackupProfile {
            name: "nightly".into(),
            mode: BackupMode::Stopped,
            keep_last: 0,
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 0,
            on_mutation: MutationPolicy::FailWorld,
            warn_seconds: 0,
        }],
    };
    let generation_handle = handle.clone();
    let backups = Arc::new(
        Backups::open(
            db.clone(),
            settings,
            &game,
            Environment {
                observe: Box::new(move || {
                    generation_handle
                        .game_snapshot()
                        .map(|s| s.generation)
                        .map_err(|_| "unproven")
                }),
                supervisor: None,
                start_gate: Default::default(),
                events: Box::new(|_| {}),
                protected: Vec::new(),
            },
        )
        .unwrap(),
    );
    backups.init_repository().unwrap();
    let files = FileBrowser::open(db.clone(), &game, &[]).unwrap();
    let automation = Automation::new(
        db.clone(),
        Arc::new(HostActions::new(
            db.clone(),
            cell,
            Some(backups.clone()),
            files,
        )),
    )
    .unwrap();
    let definition = WorkflowDefinition::from_json(
        br#"{"trigger":{"kind":"manual"},"steps":[
            {"id":"backup","action":{"kind":"backup","profile":"nightly"},"timeout_secs":60},
            {"id":"tell","action":{"kind":"notify","severity":"info","title":"Backup done","body":"ok"},"timeout_secs":10}
        ]}"#,
    )
    .unwrap();
    let workflow = automation
        .create(&owner, "Nightly", &owner.principal, definition, true)
        .unwrap();
    let run_until = |run: &str| {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            automation.tick(now()).unwrap();
            let state = automation.get_run(&owner, run).unwrap().state;
            if !matches!(state.as_str(), "running" | "queued") {
                return state;
            }
            assert!(Instant::now() < until, "run settles");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let run = automation
        .run_manual(&owner, &workflow.id)
        .unwrap()
        .expect("run created");
    assert_eq!(run_until(&run), "succeeded");
    let jobs = backups.jobs(None, 10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].requested_by, owner.principal.as_str());
    assert!(jobs[0].snapshot_id.is_some());
    // The automation device exists, is labelled, and belongs to the owner.
    let device: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT id FROM devices WHERE principal_id=? AND label=?",
                rusqlite::params![owner.principal.as_str(), AUTOMATION_DEVICE_LABEL],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    // Revoking it removes the schedules' authority: the next run is suspended.
    db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=?, revoked_by='owner' WHERE id=?",
            rusqlite::params![now(), device],
        )?;
        Ok(())
    })
    .unwrap();
    let second = automation
        .run_manual(&owner, &workflow.id)
        .unwrap()
        .expect("run created");
    assert_eq!(run_until(&second), "suspended");
    assert_eq!(
        backups.jobs(None, 10).unwrap().len(),
        1,
        "no new backup job"
    );
    // While maintenance holds the start gate, every start is refused.
    let block = runtime.start_block().block();
    assert_eq!(
        runtime
            .local_handle()
            .request(vesta_control::operations::LocalAction::Start),
        Err("local_start_refused")
    );
    assert_eq!(runtime.game_snapshot().unwrap().generation, None);
    drop(block);
    drop(automation);
    drop(backups);
    runtime.shutdown(Duration::from_secs(10)).unwrap();
}
