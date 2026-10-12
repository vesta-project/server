//! Backup jobs against an inert restic stand-in and owned temporary directories.
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vesta_control::{
    backups::{BackupError, BackupEvent, Backups, Environment, JobKind, JobState},
    config::{BackupMode, BackupProfile, BackupSettings, MutationPolicy},
    identity::{DeviceId, PrincipalId, Subject},
    operations::StartGate,
};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

const OWNER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OWNER_DEVICE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const VIEWER: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const VIEWER_DEVICE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

fn subject(principal: &str, device: &str) -> Subject {
    Subject {
        principal: PrincipalId::parse(principal).unwrap(),
        device: DeviceId::parse(device).unwrap(),
    }
}

fn owner() -> Subject {
    subject(OWNER, OWNER_DEVICE)
}

struct Fixture {
    _temp: tempfile::TempDir,
    game: PathBuf,
    db: StateDatabase,
    settings: BackupSettings,
}

fn profile(name: &str, mode: BackupMode, keep_last: u32) -> BackupProfile {
    BackupProfile {
        name: name.into(),
        mode,
        keep_last,
        keep_daily: 0,
        keep_weekly: 0,
        keep_monthly: 0,
        on_mutation: MutationPolicy::FailWorld,
        warn_seconds: 0,
    }
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    fs::create_dir_all(game.join("world/region")).unwrap();
    fs::write(game.join("world/level.dat"), b"level").unwrap();
    fs::write(game.join("world/region.mca"), b"region").unwrap();
    fs::create_dir_all(game.join("logs")).unwrap();
    fs::write(game.join("logs/latest.log"), b"log").unwrap();
    fs::write(game.join("server.properties"), b"motd=x").unwrap();
    let state = StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
    let db = StateDatabase::open(state, Arc::new(SystemClock)).unwrap();
    for (index, (principal, device, role)) in [
        (OWNER, OWNER_DEVICE, Some("owner")),
        (VIEWER, VIEWER_DEVICE, None),
    ]
    .into_iter()
    .enumerate()
    {
        db.write(|c| {
            let t = db.clock().millis();
            c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'User','active',?,?)", rusqlite::params![principal, db.server_id(), t, t])?;
            c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Device',?,?)", rusqlite::params![device, db.server_id(), principal, index.to_string().repeat(64), t])?;
            if let Some(role) = role {
                c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee',?,?,'fixture',?)", rusqlite::params![principal, role, t])?;
            }
            Ok(())
        })
        .unwrap();
    }
    let settings = BackupSettings {
        restic: PathBuf::from(env!("CARGO_BIN_EXE_vesta-restic-fixture")),
        repository: temp.path().join("repo"),
        restore_root: Some(temp.path().join("restores")),
        profiles: vec![
            profile("stopped", BackupMode::Stopped, 2),
            profile("online", BackupMode::Online, 0),
        ],
    };
    Fixture {
        _temp: temp,
        game,
        db,
        settings,
    }
}

type Events = Arc<Mutex<Vec<BackupEvent>>>;

fn open(f: &Fixture, running: bool) -> (Backups, Events, StartGate) {
    let events: Events = Arc::default();
    let sink = events.clone();
    let gate = StartGate::default();
    let generation =
        vesta_game_runtime::process::GenerationId::parse_hex("0123456789abcdef0123456789abcdef")
            .unwrap();
    let backups = Backups::open(
        f.db.clone(),
        f.settings.clone(),
        &f.game,
        Environment {
            observe: Box::new(move || Ok(running.then_some(generation))),
            supervisor: None,
            start_gate: gate.clone(),
            events: Box::new(move |event| sink.lock().unwrap().push(event)),
        },
    )
    .unwrap();
    (backups, events, gate)
}

fn wait(backups: &Backups, id: &str) -> vesta_control::backups::Job {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let job = backups.job(id).unwrap().unwrap();
        if job.state.finished() {
            return job;
        }
        assert!(Instant::now() < until, "job finishes");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn restore(snapshot: &str, paths: &[&str]) -> JobKind {
    JobKind::Restore {
        snapshot: snapshot.into(),
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
    }
}

#[test]
fn stopped_backup_snapshots_applies_retention_ignores_and_restores_for_inspection() {
    let f = fixture();
    fs::write(f.game.join(".vesta-backupignore"), "*.log\n").unwrap();
    let (backups, events, gate) = open(&f, false);
    backups.init_repository().unwrap();
    let mut snapshots = Vec::new();
    for round in 0..3 {
        let key = format!("k{round}");
        let id = backups
            .submit(&owner(), "stopped", JobKind::Backup, Some(&key))
            .unwrap();
        let job = wait(&backups, &id);
        assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error_code);
        assert_eq!(job.result.as_ref().unwrap()["consistency"], "stopped");
        snapshots.push(job.snapshot_id.unwrap());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!gate.blocked(), "start is unblocked afterwards");
    // Retention keeps the last two validated restore points.
    let kept: Vec<String> = backups
        .snapshots("stopped")
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(kept, snapshots[1..]);
    // The same idempotency key and request returns the same job.
    let first_job = backups.jobs(None, 10).unwrap().last().unwrap().id.clone();
    assert_eq!(
        backups
            .submit(&owner(), "stopped", JobKind::Backup, Some("k0"))
            .unwrap(),
        first_job
    );
    // A different request under the same key is a conflict, not a silent dedupe.
    assert_eq!(
        backups.submit(&owner(), "stopped", JobKind::Verify, Some("k0")),
        Err(BackupError::IdempotencyConflict)
    );
    // A user without backups.create is refused.
    assert_eq!(
        backups.submit(
            &subject(VIEWER, VIEWER_DEVICE),
            "stopped",
            JobKind::Backup,
            Some("k0")
        ),
        Err(BackupError::Forbidden)
    );
    // Composite cursor pages through every job exactly once.
    let all = backups.jobs(None, 10).unwrap();
    let first = backups.jobs(None, 1).unwrap();
    let rest = backups
        .jobs(Some((first[0].created_at, first[0].id.as_str())), 10)
        .unwrap();
    assert_eq!(1 + rest.len(), all.len());
    // Restore paths are literal; pattern syntax is refused.
    assert_eq!(
        backups.submit(
            &owner(),
            "stopped",
            restore(&snapshots[2], &["world/[ab].dat"]),
            None
        ),
        Err(BackupError::Invalid)
    );
    // Inspection restore: a fresh directory, selected paths only.
    let id = backups
        .submit(
            &owner(),
            "stopped",
            restore(&snapshots[2], &["world"]),
            None,
        )
        .unwrap();
    let job = wait(&backups, &id);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error_code);
    let restored = PathBuf::from(job.result.unwrap()["restored_to"].as_str().unwrap());
    assert_eq!(
        fs::read(restored.join("world/level.dat")).unwrap(),
        b"level"
    );
    assert!(!restored.join("server.properties").exists());
    let verify = backups
        .submit(&owner(), "stopped", JobKind::Verify, None)
        .unwrap();
    assert_eq!(wait(&backups, &verify).state, JobState::Succeeded);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, BackupEvent::Completed { .. }))
            .count(),
        3
    );
    // Logs were excluded from the snapshot.
    let repo_logs = f
        .settings
        .repository
        .join("snapshots")
        .join(&snapshots[2])
        .join("data/logs/latest.log");
    assert!(!repo_logs.exists());
}

#[test]
fn modes_and_world_mutations_decide_failure_without_touching_the_game() {
    let f = fixture();
    // A stopped backup refuses while the game runs; online needs a supervisor.
    let (backups, events, gate) = open(&f, true);
    backups.init_repository().unwrap();
    let stopped = backups
        .submit(&owner(), "stopped", JobKind::Backup, None)
        .unwrap();
    let job = wait(&backups, &stopped);
    assert_eq!(
        job.error_code.as_deref(),
        Some("backup_requires_stopped_server")
    );
    assert!(!gate.blocked());
    let online = backups
        .submit(&owner(), "online", JobKind::Backup, None)
        .unwrap();
    assert_eq!(
        wait(&backups, &online).error_code.as_deref(),
        Some("backup_online_requires_supervisor")
    );
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(BackupEvent::Failed { .. })
    ));
    drop(backups);
    // An independent write to world data during the scan fails the safe preset, and
    // the rejected snapshot never becomes a restore point.
    let (backups, _, _) = open(&f, false);
    fs::write(f.game.join(".fake-mutate"), b"1").unwrap();
    let id = backups
        .submit(&owner(), "stopped", JobKind::Backup, None)
        .unwrap();
    let job = wait(&backups, &id);
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("backup_source_changed"));
    let result = job.result.unwrap();
    assert_eq!(result["world_changed"], true);
    assert!(
        result["changed_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "world/region.mca")
    );
    assert!(backups.snapshots("stopped").unwrap().is_empty());
    // Unknown profiles and malformed restores are refused up front.
    assert!(
        backups
            .submit(&owner(), "nope", JobKind::Backup, None)
            .is_err()
    );
    for request in [
        restore("../escape", &[]),
        restore("0123456789abcdef", &["../x"]),
    ] {
        assert!(backups.submit(&owner(), "stopped", request, None).is_err());
    }
}

#[test]
fn a_queued_job_is_refused_when_its_submitter_lost_authority() {
    let f = fixture();
    let (backups, _, _) = open(&f, false);
    backups.init_repository().unwrap();
    // Revoke the submitting device after admission but before the worker runs it.
    f.db.write(|c| {
        c.execute(
            "INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,created_at) VALUES ('q1','stopped','backup','queued',?,?,1)",
            rusqlite::params![OWNER, VIEWER_DEVICE],
        )?;
        Ok(())
    })
    .unwrap();
    let job = wait(&backups, "q1");
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("backup_authority_revoked"));
}

#[test]
fn a_job_interrupted_by_an_agent_crash_is_marked_uncertain() {
    let f = fixture();
    f.db.write(|c| {
        c.execute(
            "INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,created_at,started_at) VALUES ('j1','stopped','backup','running',?,?,1,1)",
            rusqlite::params![OWNER, OWNER_DEVICE],
        )?;
        Ok(())
    })
    .unwrap();
    let (backups, _, _) = open(&f, false);
    let job = backups.job("j1").unwrap().unwrap();
    assert_eq!(job.state, JobState::Uncertain);
    assert_eq!(job.error_code.as_deref(), Some("backup_interrupted"));
}
