//! Backup jobs with the built-in engine over owned temporary directories.
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vesta_control::{
    backups::store::{LocalStore, ObjectInfo, Store},
    backups::{BackupError, BackupEvent, Backups, Environment, JobKind, JobState},
    config::{BackupMode, BackupProfile, BackupRepository, BackupSettings, MutationPolicy},
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
        repository: BackupRepository::Local(temp.path().join("repo")),
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

/// A local store that, once, writes to world data while the backup is reading:
/// an independent writer the backup must detect.
struct MutatingStore {
    inner: LocalStore,
    game: PathBuf,
    done: std::sync::atomic::AtomicBool,
}
impl Store for MutatingStore {
    fn put(&self, key: &str, bytes: &[u8]) -> vesta_control::backups::store::Result<()> {
        if key.starts_with("chunks/") && !self.done.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            fs::write(self.game.join("world/region.mca"), b"changed during backup").unwrap();
        }
        self.inner.put(key, bytes)
    }
    fn get(&self, key: &str) -> vesta_control::backups::store::Result<Vec<u8>> {
        self.inner.get(key)
    }
    fn exists(&self, key: &str) -> vesta_control::backups::store::Result<bool> {
        self.inner.exists(key)
    }
    fn list(
        &self,
        prefix: &str,
        limit: usize,
    ) -> vesta_control::backups::store::Result<Vec<ObjectInfo>> {
        self.inner.list(prefix, limit)
    }
    fn delete(&self, key: &str) -> vesta_control::backups::store::Result<()> {
        self.inner.delete(key)
    }
}

fn open(f: &Fixture, running: bool) -> (Backups, Events, StartGate) {
    open_with(f, running, false)
}

fn open_with(f: &Fixture, running: bool, mutate: bool) -> (Backups, Events, StartGate) {
    let events: Events = Arc::default();
    let sink = events.clone();
    let gate = StartGate::default();
    let generation =
        vesta_game_runtime::process::GenerationId::parse_hex("0123456789abcdef0123456789abcdef")
            .unwrap();
    let BackupRepository::Local(repo) = &f.settings.repository else {
        unreachable!()
    };
    let store: Box<dyn Store> = if mutate {
        Box::new(MutatingStore {
            inner: LocalStore::new(repo.clone()),
            game: f.game.clone(),
            done: Default::default(),
        })
    } else {
        Box::new(LocalStore::new(repo.clone()))
    };
    let backups = Backups::open_with_store(
        f.db.clone(),
        f.settings.clone(),
        store,
        &f.game,
        Environment {
            observe: Box::new(move || Ok(running.then_some(generation))),
            supervisor: None,
            start_gate: gate.clone(),
            events: Box::new(move |event| sink.lock().unwrap().push(event)),
            protected: vec![f.game.join("private")],
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
    // A full restore has everything except the ignored logs.
    let id = backups
        .submit(&owner(), "stopped", restore(&snapshots[2], &[]), None)
        .unwrap();
    let job = wait(&backups, &id);
    let restored = PathBuf::from(job.result.unwrap()["restored_to"].as_str().unwrap());
    assert!(restored.join("server.properties").is_file());
    assert!(!restored.join("logs/latest.log").exists());
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
    let (backups, _, _) = open_with(&f, false, true);
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

fn tree(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn backup_now(backups: &Backups, profile: &str) -> String {
    let id = backups
        .submit(&owner(), profile, JobKind::Backup, None)
        .unwrap();
    let job = wait(backups, &id);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error_code);
    job.snapshot_id.unwrap()
}

fn restore_in_place(
    backups: &Backups,
    snapshot: &str,
    paths: &[&str],
) -> vesta_control::backups::Job {
    let prepared = backups
        .prepare_restore(
            &owner(),
            "stopped",
            snapshot,
            paths.iter().map(|p| (*p).to_owned()).collect(),
            false,
        )
        .unwrap();
    backups
        .confirm_restore(&owner(), &prepared.job_id, &prepared.token)
        .unwrap();
    wait(backups, &prepared.job_id)
}

#[test]
fn in_place_restore_needs_exact_confirmation_and_can_be_undone_from_its_safety_snapshot() {
    let f = fixture();
    fs::write(f.game.join(".vesta-backupignore"), "logs/\n").unwrap();
    fs::create_dir_all(f.game.join("private")).unwrap();
    fs::write(f.game.join("private/key"), b"key-1").unwrap();
    let (backups, _, gate) = open(&f, false);
    backups.init_repository().unwrap();
    let snapshot = backup_now(&backups, "stopped");
    let original = tree(&f.game);

    // Later changes: an edit, a new file, a newer log (ignored) and a rotated key
    // (protected).
    fs::write(f.game.join("world/level.dat"), b"griefed").unwrap();
    fs::write(f.game.join("world/extra.dat"), b"extra").unwrap();
    fs::write(f.game.join("logs/latest.log"), b"newer log").unwrap();
    fs::write(f.game.join("private/key"), b"key-2").unwrap();
    let changed = tree(&f.game);

    // Restoring needs backups.restore.
    let viewer = subject(VIEWER, VIEWER_DEVICE);
    assert_eq!(
        backups
            .prepare_restore(&viewer, "stopped", &snapshot, vec![], false)
            .unwrap_err(),
        BackupError::Forbidden
    );
    // A prepared restore does nothing until it is confirmed.
    let prepared = backups
        .prepare_restore(&owner(), "stopped", &snapshot, vec![], false)
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let job = backups.job(&prepared.job_id).unwrap().unwrap();
    assert_eq!(job.state, JobState::AwaitingConfirmation);
    assert_eq!(tree(&f.game), changed);
    // Only the same principal and device can confirm; that refusal burns nothing.
    assert_eq!(
        backups.confirm_restore(&viewer, &prepared.job_id, &prepared.token),
        Err(BackupError::ConfirmationInvalid)
    );
    assert_eq!(
        backups.job(&prepared.job_id).unwrap().unwrap().state,
        JobState::AwaitingConfirmation
    );
    // A wrong token burns the request.
    let wrong = vesta_control::operations::ConfirmationToken::try_from("A".repeat(43)).unwrap();
    assert_eq!(
        backups.confirm_restore(&owner(), &prepared.job_id, &wrong),
        Err(BackupError::ConfirmationInvalid)
    );
    assert_eq!(
        backups.confirm_restore(&owner(), &prepared.job_id, &prepared.token),
        Err(BackupError::ConfirmationInvalid)
    );
    let burned = backups.job(&prepared.job_id).unwrap().unwrap();
    assert_eq!(burned.state, JobState::Failed);
    assert_eq!(
        burned.error_code.as_deref(),
        Some("backup_confirmation_invalid")
    );
    assert_eq!(tree(&f.game), changed);

    // Confirmed: the world returns, ignored and protected paths stay as they are.
    let job = restore_in_place(&backups, &snapshot, &[]);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error_code);
    let result = job.result.unwrap();
    let mut expected = original.clone();
    expected.insert("logs/latest.log".into(), b"newer log".to_vec());
    expected.insert("private/key".into(), b"key-2".to_vec());
    assert_eq!(tree(&f.game), expected);
    assert!(!gate.blocked());

    // The safety snapshot holds the state just before the restore and undoes it.
    let safety = result["safety_snapshot_id"].as_str().unwrap().to_owned();
    let listed: Vec<String> = backups
        .safety_snapshots("stopped")
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(listed, vec![safety.clone()]);
    assert!(
        backups
            .snapshots("stopped")
            .unwrap()
            .iter()
            .all(|s| s.id != safety),
        "safety snapshots are not profile restore points"
    );
    let undo = restore_in_place(&backups, &safety, &[]);
    assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.error_code);
    assert_eq!(tree(&f.game), changed);

    // A selective restore changes only that path.
    let partial = restore_in_place(&backups, &snapshot, &["world/level.dat"]);
    assert_eq!(
        partial.state,
        JobState::Succeeded,
        "{:?}",
        partial.error_code
    );
    assert_eq!(fs::read(f.game.join("world/level.dat")).unwrap(), b"level");
    assert!(f.game.join("world/extra.dat").exists());
}

#[test]
fn in_place_restore_refuses_a_running_game_and_snapshots_that_are_not_restore_points() {
    let f = fixture();
    let (backups, _, _) = open(&f, false);
    backups.init_repository().unwrap();
    let snapshot = backup_now(&backups, "stopped");
    fs::write(f.game.join("world/level.dat"), b"changed").unwrap();
    let before = tree(&f.game);
    drop(backups);

    // A running game refuses before anything changes.
    let (running, _, gate) = open(&f, true);
    let job = restore_in_place(&running, &snapshot, &[]);
    assert_eq!(
        job.error_code.as_deref(),
        Some("backup_requires_stopped_server")
    );
    assert_eq!(tree(&f.game), before);
    assert!(!gate.blocked());
    drop(running);

    // A snapshot rejected for inconsistency, or unknown, is never restored.
    let (mutating, _, _) = open_with(&f, false, true);
    let id = mutating
        .submit(&owner(), "stopped", JobKind::Backup, None)
        .unwrap();
    let rejected = wait(&mutating, &id);
    assert_eq!(
        rejected.error_code.as_deref(),
        Some("backup_source_changed")
    );
    let rejected = rejected.snapshot_id.unwrap();
    drop(mutating);
    let (backups, _, _) = open(&f, false);
    let before = tree(&f.game);
    let job = restore_in_place(&backups, &rejected, &[]);
    assert_eq!(
        job.error_code.as_deref(),
        Some("backup_snapshot_not_restorable")
    );
    let job = restore_in_place(&backups, "0123456789abcdef0123456789abcdef", &[]);
    assert_eq!(job.error_code.as_deref(), Some("backup_snapshot_unknown"));
    let job = restore_in_place(&backups, &snapshot, &["world/missing.dat"]);
    assert_eq!(
        job.error_code.as_deref(),
        Some("backup_restore_path_not_in_snapshot")
    );
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(tree(&f.game), before);

    // An expired confirmation can no longer be redeemed.
    let prepared = backups
        .prepare_restore(&owner(), "stopped", &snapshot, vec![], true)
        .unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE backup_jobs SET confirm_by=1 WHERE id=?",
            rusqlite::params![prepared.job_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        backups.confirm_restore(&owner(), &prepared.job_id, &prepared.token),
        Err(BackupError::ConfirmationInvalid)
    );
    let expired = backups.job(&prepared.job_id).unwrap().unwrap();
    assert_eq!(
        expired.error_code.as_deref(),
        Some("backup_confirmation_expired")
    );
}

/// What a crashed agent leaves behind: a running in-place restore that was applying.
fn interrupt(f: &Fixture, id: &str) {
    f.db.write(|c| {
        c.execute(
            "INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,target,created_at,started_at,applying) VALUES (?,'stopped','restore_in_place','running',?,?,'{}',1,1,1)",
            rusqlite::params![id, OWNER, OWNER_DEVICE],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn an_interrupted_restore_blocks_starts_until_resolved_or_superseded() {
    let f = fixture();
    let (backups, _, _) = open(&f, false);
    backups.init_repository().unwrap();
    let snapshot = backup_now(&backups, "stopped");
    drop(backups);

    interrupt(&f, "interrupted-1");
    let (backups, _, gate) = open(&f, false);
    let job = backups.job("interrupted-1").unwrap().unwrap();
    assert_eq!(job.state, JobState::RecoveryRequired);
    assert_eq!(
        job.error_code.as_deref(),
        Some("backup_restore_interrupted")
    );
    assert!(gate.blocked(), "starts are blocked on open");
    assert!(backups.recovery_required());
    // Resolving needs backups.restore; then starts are allowed again.
    assert_eq!(
        backups.resolve_recovery(&subject(VIEWER, VIEWER_DEVICE), "interrupted-1"),
        Err(BackupError::Forbidden)
    );
    assert!(gate.blocked());
    backups.resolve_recovery(&owner(), "interrupted-1").unwrap();
    assert!(!gate.blocked());
    assert!(!backups.recovery_required());
    assert_eq!(
        backups.resolve_recovery(&owner(), "interrupted-1"),
        Err(BackupError::UnknownJob)
    );
    drop(backups);

    // A later complete restore also clears it.
    interrupt(&f, "interrupted-2");
    let (backups, _, gate) = open(&f, false);
    assert!(gate.blocked());
    let job = restore_in_place(&backups, &snapshot, &[]);
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error_code);
    assert!(!gate.blocked());
    let cleared = backups.job("interrupted-2").unwrap().unwrap();
    assert_eq!(cleared.state, JobState::Failed);
    assert_eq!(cleared.result.unwrap()["recovery"], "superseded_by_restore");
}
