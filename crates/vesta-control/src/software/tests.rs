use super::*;
use rusqlite::params;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use vesta_server_install::{InstallRoots, ManagedLaunch, TransportConfig};
use vesta_state::{StateDirectory, SystemClock};

const PRINCIPAL: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const DEVICE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const DEVICE2: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const VIEWER: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const VIEWER_DEVICE: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";

#[derive(Default)]
struct NativeState {
    running: AtomicBool,
    fail_after_write: AtomicBool,
    blocked: AtomicBool,
    calls: AtomicUsize,
    document: Mutex<String>,
}
struct Hooks(Arc<NativeState>);
struct Guard(Arc<NativeState>);
impl RuntimeHooks for Hooks {
    fn launch_document(&self, _: &ManagedLaunch) -> Result<String> {
        Ok("candidate launch".into())
    }
    fn acquire_stopped(&self) -> Result<Box<dyn StoppedGuard>> {
        if self.0.running.load(Ordering::SeqCst) {
            Err(SoftwareError::Busy)
        } else {
            Ok(Box::new(Guard(self.0.clone())))
        }
    }
    fn recovery_block(&self, blocked: bool) -> Result<()> {
        self.0.blocked.store(blocked, Ordering::SeqCst);
        Ok(())
    }
}
impl StoppedGuard for Guard {
    fn activate(&mut self, document: &str) -> Result<()> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        *self.0.document.lock().unwrap() = document.into();
        if self.0.fail_after_write.load(Ordering::SeqCst) {
            Err(SoftwareError::RuntimeUnavailable)
        } else {
            Ok(())
        }
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    db: StateDatabase,
    game: PathBuf,
    installer: Arc<ServerInstaller>,
    native: Arc<NativeState>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        std::fs::write(game.join("server.properties"), b"motd=preserved\n").unwrap();
        std::fs::create_dir(game.join("world")).unwrap();
        std::fs::write(game.join("world/level.dat"), b"world unchanged").unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        for (index, (principal, device, role)) in [
            (PRINCIPAL, DEVICE, Some("owner")),
            (VIEWER, VIEWER_DEVICE, Some("viewer")),
        ]
        .into_iter()
        .enumerate()
        {
            db.write(|c| {
                let now=db.clock().millis();
                c.execute("INSERT INTO principals(id,server_id,display_name,status,created_at,updated_at) VALUES(?,?,'fixture','active',?,?)",params![principal,db.server_id(),now,now])?;
                c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,'fixture',?,?)",params![device,db.server_id(),principal,index.to_string().repeat(64),now])?;
                c.execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES(?,?,?,'fixture',?)",params![uuid::Uuid::new_v4().to_string(),principal,role,now])?;
                Ok(())
            }).unwrap();
        }
        db.write(|c| {c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,'second',?,?)",params![DEVICE2,db.server_id(),PRINCIPAL,"f".repeat(64),db.clock().millis()])?;Ok(())}).unwrap();
        let base = temp.path().join("software");
        std::fs::create_dir(&base).unwrap();
        let installer = Arc::new(
            ServerInstaller::new(
                InstallRoots::new(&base, &[game.clone()]).unwrap(),
                TransportConfig::direct(),
            )
            .unwrap(),
        );
        let native = Arc::new(NativeState::default());
        *native.document.lock().unwrap() = "original launch".into();
        Self {
            _temp: temp,
            db,
            game,
            installer,
            native,
        }
    }
    fn open(&self) -> SoftwareService {
        SoftwareService::open(
            self.db.clone(),
            self.installer.clone(),
            std::env::current_exe().unwrap(),
            self.game.clone(),
            self.native.document.lock().unwrap().clone(),
            Arc::new(Hooks(self.native.clone())),
        )
        .unwrap()
    }
    fn previous(&self, document: &str) {
        let launch = StoredLaunch {
            installation: Installation {
                software: Some(Software::Paper),
                version: Some("1.21.4".into()),
                build: Some(1),
                installation_id: "srv_previous".into(),
            },
            document: document.into(),
            artifacts: vec![],
        };
        self.db
            .write(|c| {
                c.execute(
                    "UPDATE software_state SET previous_launch=? WHERE singleton=1",
                    [store::encode(&launch).unwrap()],
                )?;
                Ok(())
            })
            .unwrap();
    }
    fn unchanged(&self) {
        assert_eq!(
            std::fs::read(self.game.join("server.properties")).unwrap(),
            b"motd=preserved\n"
        );
        assert_eq!(
            std::fs::read(self.game.join("world/level.dat")).unwrap(),
            b"world unchanged"
        );
    }
}
fn actor(device: &str) -> Subject {
    Subject {
        principal: crate::identity::PrincipalId::parse(PRINCIPAL).unwrap(),
        device: crate::identity::DeviceId::parse(device).unwrap(),
    }
}
fn owner() -> Subject {
    actor(DEVICE)
}
fn viewer() -> Subject {
    Subject {
        principal: crate::identity::PrincipalId::parse(VIEWER).unwrap(),
        device: crate::identity::DeviceId::parse(VIEWER_DEVICE).unwrap(),
    }
}
fn request(p: &PreparedRollback, key: &str) -> RollbackRequest {
    RollbackRequest {
        expected_revision: p.expected_revision,
        idempotency_key: key.into(),
        confirm: true,
        plan_id: p.plan_id.clone(),
        confirmation_token: p.confirmation_token.clone(),
    }
}
fn finished(handle: &SoftwareHandle, id: &str) -> Job {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let job = handle
            .jobs(&owner())
            .unwrap()
            .into_iter()
            .find(|j| j.id == id)
            .unwrap();
        if !matches!(job.state.as_str(), "queued" | "staging" | "activating") {
            return job;
        }
        assert!(Instant::now() < end, "software worker did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn navigation_has_no_lifecycle_or_provider_effects_and_roles_are_rechecked() {
    let f = Fixture::new();
    let service = f.open();
    let h = service.handle();
    assert_eq!(h.status(&owner()).unwrap().revision, 0);
    assert!(h.jobs(&owner()).unwrap().is_empty());
    assert_eq!(f.native.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(h.status(&viewer()), Err(SoftwareError::Forbidden)));
    assert!(matches!(h.jobs(&viewer()), Err(SoftwareError::Forbidden)));
    assert!(matches!(
        h.prepare_rollback(&viewer(), 0),
        Err(SoftwareError::Forbidden)
    ));
    f.unchanged();
}

#[test]
fn rollback_is_actor_bound_single_use_and_idempotent_after_restart() {
    let f = Fixture::new();
    let service = f.open();
    let h = service.handle();
    f.previous("previous launch");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    assert!(matches!(
        h.rollback(&actor(DEVICE2), request(&p, "rollback-1")),
        Err(SoftwareError::ConfirmationInvalid)
    ));
    let first = h.rollback(&owner(), request(&p, "rollback-1")).unwrap();
    let job = finished(&h, &first.id);
    assert_eq!(job.state, "succeeded");
    assert_eq!(h.status(&owner()).unwrap().revision, 1);
    assert_eq!(*f.native.document.lock().unwrap(), "previous launch");
    assert_eq!(
        h.rollback(&owner(), request(&p, "rollback-1")).unwrap().id,
        first.id
    );
    assert!(matches!(
        h.rollback(&owner(), request(&p, "rollback-2")),
        Err(SoftwareError::PlanExpired)
    ));
    drop(service);
    let reopened = f.open();
    let h = reopened.handle();
    assert_eq!(
        h.rollback(&owner(), request(&p, "rollback-1")).unwrap().id,
        first.id
    );
    let mut changed = request(&p, "rollback-1");
    changed.expected_revision = 1;
    assert!(matches!(
        h.rollback(&owner(), changed),
        Err(SoftwareError::IdempotencyConflict)
    ));
    assert_eq!(f.native.calls.load(Ordering::SeqCst), 1);
    assert!(!f.native.blocked.load(Ordering::SeqCst));
    f.unchanged();
}

#[test]
fn wrong_confirmation_burns_only_same_actor_plan() {
    let f = Fixture::new();
    let service = f.open();
    let h = service.handle();
    f.previous("previous");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let mut wrong = request(&p, "rollback-1");
    wrong.confirmation_token = "wrong".into();
    assert!(matches!(
        h.rollback(&owner(), wrong),
        Err(SoftwareError::ConfirmationInvalid)
    ));
    assert!(matches!(
        h.rollback(&owner(), request(&p, "rollback-2")),
        Err(SoftwareError::PlanExpired)
    ));
    assert_eq!(f.native.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn running_game_refuses_activation_without_changing_launch_or_world() {
    let f = Fixture::new();
    let service = f.open();
    let h = service.handle();
    f.previous("previous");
    f.native.running.store(true, Ordering::SeqCst);
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let job = h.rollback(&owner(), request(&p, "rollback-1")).unwrap();
    let done = finished(&h, &job.id);
    assert_eq!(done.state, "failed");
    assert_eq!(done.error.as_deref(), Some("software_busy"));
    assert_eq!(h.status(&owner()).unwrap().revision, 0);
    assert_eq!(*f.native.document.lock().unwrap(), "original launch");
    assert!(!f.native.blocked.load(Ordering::SeqCst));
    f.unchanged();
}

#[test]
fn partial_activation_survives_restart_and_requires_explicit_rollback() {
    let f = Fixture::new();
    let service = f.open();
    let h = service.handle();
    f.previous("recoverable previous");
    f.native.fail_after_write.store(true, Ordering::SeqCst);
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let job = h.rollback(&owner(), request(&p, "rollback-1")).unwrap();
    assert_eq!(finished(&h, &job.id).state, "recovery_required");
    assert!(f.native.blocked.load(Ordering::SeqCst));
    assert_eq!(h.status(&owner()).unwrap().revision, 0);
    drop(service);
    let reopened = f.open();
    let h = reopened.handle();
    assert!(h.status(&owner()).unwrap().recovery_required);
    assert!(f.native.blocked.load(Ordering::SeqCst));
    f.native.fail_after_write.store(false, Ordering::SeqCst);
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let next = h.rollback(&owner(), request(&p, "rollback-2")).unwrap();
    assert_eq!(finished(&h, &next.id).state, "succeeded");
    assert!(!h.status(&owner()).unwrap().recovery_required);
    assert!(!f.native.blocked.load(Ordering::SeqCst));
    f.unchanged();
}

#[test]
fn interrupted_staging_is_never_replayed_and_unknown_launch_blocks_start() {
    let f = Fixture::new();
    let service = f.open();
    drop(service);
    f.db.write(|c| {c.execute("INSERT INTO software_jobs(id,kind,state,principal,device,idempotency_key,request_hash,expected_revision,created_at_ms,updated_at_ms) VALUES('interrupted','apply','staging',?,?,'some-key','some-hash',0,1,1)",params![PRINCIPAL,DEVICE])?;Ok(())}).unwrap();
    let service = f.open();
    let h = service.handle();
    assert_eq!(h.jobs(&owner()).unwrap()[0].state, "failed");
    assert!(!h.status(&owner()).unwrap().recovery_required);
    drop(service);
    *f.native.document.lock().unwrap() = "unexpected local edit".into();
    let service = f.open();
    assert!(service.handle().status(&owner()).unwrap().recovery_required);
    assert!(f.native.blocked.load(Ordering::SeqCst));
    assert_eq!(f.native.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn revoked_owner_cannot_replay_durable_jobs() {
    let f = Fixture::new();
    let service = f.open();
    let h = service.handle();
    f.previous("previous");
    let p = h.prepare_rollback(&owner(), 0).unwrap();
    let job = h.rollback(&owner(), request(&p, "rollback-1")).unwrap();
    assert_eq!(finished(&h, &job.id).state, "succeeded");
    f.db.write(|c| {
        c.execute(
            "DELETE FROM role_assignments WHERE principal_id=?",
            [PRINCIPAL],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        h.rollback(&owner(), request(&p, "rollback-1")),
        Err(SoftwareError::Forbidden)
    ));
    assert!(matches!(h.status(&owner()), Err(SoftwareError::Forbidden)));
}

#[test]
fn remote_path_url_and_unimplemented_provider_inputs_are_rejected() {
    for value in [
        serde_json::json!({"software":"paper","version":"1.21.4","java":"C:/attacker.exe"}),
        serde_json::json!({"software":"paper","version":"1.21.4","url":"https://attacker.invalid"}),
    ] {
        assert!(serde_json::from_value::<PlanRequest>(value).is_err());
    }
    for software in [Software::Forge, Software::NeoForge] {
        assert!(!software.descriptor().automatic_backend);
        assert!(
            validate_request(&PlanRequest {
                software,
                version: "1.21.4".into(),
                build: None,
                loader_version: None,
                experimental: false
            })
            .is_err()
        );
    }
    for version in ["../../world", "https://example.com/jar", "1.21\nstop", ""] {
        assert!(
            validate_request(&PlanRequest {
                software: Software::Vanilla,
                version: version.into(),
                build: None,
                loader_version: None,
                experimental: false
            })
            .is_err()
        );
    }
}
