use super::*;
use crate::machine::imports::browse::{ImportRootConfig, ResolveRequest};
use crate::worker::ServerWorker;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use vesta_state::SystemClock;

const GIB: u64 = 1024 * 1024 * 1024;

type Hook = Box<dyn Fn(u64) + Send>;

/// Deterministic host observations.
pub(super) struct FakeEnv {
    pub running: AtomicBool,
    pub unknown: AtomicBool,
    pub busy_port: Mutex<Option<u16>>,
    /// Free bytes reported; after `low_after` calls, report zero.
    pub free: AtomicU64,
    pub free_calls: AtomicU64,
    pub low_after: AtomicU64,
    pub hook: Mutex<Option<Hook>>,
    pub java: Mutex<PathBuf>,
}
impl Default for FakeEnv {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            unknown: AtomicBool::new(false),
            busy_port: Mutex::new(None),
            free: AtomicU64::new(100 * GIB),
            free_calls: AtomicU64::new(0),
            low_after: AtomicU64::new(u64::MAX),
            hook: Mutex::new(None),
            java: Mutex::new(PathBuf::new()),
        }
    }
}
impl ImportEnvironment for FakeEnv {
    fn processes(&self, _: &[PathBuf]) -> (Vec<ProcessEvidence>, &'static str) {
        if self.running.load(Ordering::SeqCst) {
            (
                vec![ProcessEvidence {
                    kind: "java",
                    pid: 42,
                    name: "java".into(),
                    start_identity: Some("linux:42:1".into()),
                    relation: "source",
                    state: "running",
                }],
                "complete",
            )
        } else {
            (
                Vec::new(),
                if self.unknown.load(Ordering::SeqCst) {
                    "unknown"
                } else {
                    "complete"
                },
            )
        }
    }
    fn port_state(&self, port: u16) -> &'static str {
        if *self.busy_port.lock().unwrap() == Some(port) {
            "occupied"
        } else {
            "free"
        }
    }
    fn free_bytes(&self, _: &Path) -> Option<u64> {
        let calls = self.free_calls.fetch_add(1, Ordering::SeqCst);
        if calls >= self.low_after.load(Ordering::SeqCst) {
            Some(0)
        } else {
            Some(self.free.load(Ordering::SeqCst))
        }
    }
    fn memory_total_bytes(&self) -> Option<u64> {
        Some(16 * GIB)
    }
    fn plan_java(&self, version: Option<&str>, _: &[PathBuf]) -> HostJavaPlan {
        crate::java::plan_from(&[], version)
    }
    fn ensure_java(&self, runtime: &ReviewRuntime, _: &[PathBuf]) -> Result<PathBuf, &'static str> {
        runtime.required_major.ok_or("java_unresolved")?;
        Ok(self.java.lock().unwrap().clone())
    }
    fn before_copy_entry(&self, index: u64) {
        if let Some(hook) = &*self.hook.lock().unwrap_or_else(|e| e.into_inner()) {
            hook(index);
        }
    }
}
impl FakeEnv {
    pub fn set_hook(&self, hook: Option<Hook>) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = hook;
    }
}

pub(super) struct Fixture {
    pub temp: tempfile::TempDir,
    pub state: HostState,
    pub browser: Arc<FolderBrowser>,
    pub env: Arc<FakeEnv>,
    pub jobs: ImportJobs,
    pub binding: OwnerBinding,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        for dir in ["imports", "servers", "host"] {
            fs::create_dir_all(base.join(dir)).unwrap();
        }
        let state = HostState::open(base.join("host"), Arc::new(SystemClock)).unwrap();
        let account = Uuid::new_v4();
        let device = state.claim(account, "Laptop", [7; 32]).unwrap();
        let binding = OwnerBinding {
            host_id: state.snapshot().unwrap().host_id.to_string(),
            account_id: account.to_string(),
            device_id: device.device_id.to_string(),
        };
        let browser = Arc::new(
            FolderBrowser::new(
                vec![ImportRootConfig {
                    root_id: "r1".into(),
                    label: "Imports".into(),
                    path: base.join("imports"),
                }],
                vec![base.join("servers"), base.join("host")],
            )
            .unwrap(),
        );
        let env = Arc::new(FakeEnv::default());
        fs::create_dir_all(base.join("runtime/bin")).unwrap();
        fs::write(base.join("runtime/bin/java"), b"").unwrap();
        *env.java.lock().unwrap() = fs::canonicalize(base.join("runtime/bin/java")).unwrap();
        let jobs = ImportJobs::open(
            state.clone(),
            browser.clone(),
            &base.join("servers"),
            env.clone(),
        )
        .unwrap();
        Self {
            temp,
            state,
            browser,
            env,
            jobs,
            binding,
        }
    }
    pub fn reopen(&mut self) {
        self.jobs = ImportJobs::open(
            self.state.clone(),
            self.browser.clone(),
            &self.temp.path().join("servers"),
            self.env.clone(),
        )
        .unwrap();
    }
    pub fn source(&self, name: &str) -> PathBuf {
        self.temp.path().join("imports").join(name)
    }
    pub fn put(&self, relative: &str, text: &str) {
        let path = self.temp.path().join("imports").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = if relative.ends_with("server.properties") {
            let port = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap().local_addr().unwrap().port();
            text.replace("server-port=25565", &format!("server-port={port}"))
        } else { text.to_owned() };
        fs::write(path, text).unwrap();
    }
    pub fn vanilla(&self) {
        self.put("vanilla/server.properties", "server-port=25565\nmotd=hi\n");
        self.put("vanilla/minecraft_server.1.21.1.jar", "jar");
        self.put("vanilla/world/level.dat", "nbt");
        self.put("vanilla/world/region/r.0.0.mca", "region");
        fs::File::options()
            .write(true)
            .open(self.source("vanilla").join("world/region/r.0.0.mca"))
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(120),
                ),
            )
            .unwrap();
        self.put("vanilla/eula.txt", "eula=false\n");
    }
    pub fn fabric(&self) {
        self.put("fabric/server.properties", "");
        self.put("fabric/fabric-server-launch.jar", "launcher");
        self.put("fabric/.fabric/remappedJars/minecraft-1.20.1-0.15.jar", "x");
        self.put("fabric/mods/sodium.jar", "mod");
        self.put("fabric/world/level.dat", "nbt");
    }
    pub fn token(&self, folder: &str) -> String {
        self.browser
            .resolve(
                &self.binding,
                &ResolveRequest {
                    root_id: "r1".into(),
                    relative_path: folder.into(),
                },
            )
            .unwrap()
            .folder_token
    }
    pub fn plan(&self, folder: &str) -> ImportJob {
        self.jobs
            .plan(
                &self.binding,
                PlanRequest {
                    folder_token: self.token(folder),
                    name: "Survival".into(),
                    game_port: None,
                    memory_mib: Some(2048),
                    launch_choice: None,
                    minecraft_version: None,
                },
            )
            .unwrap()
    }
    pub fn apply_request(job: &ImportJob) -> ApplyRequest {
        let review = job.review.as_ref().unwrap();
        ApplyRequest {
            plan_id: review.plan_id.clone(),
            confirmation: review.confirmation.clone().unwrap(),
            acknowledged_warnings: review.warnings.clone(),
            eula_accepted: true,
            source_stopped_confirmed: true,
        }
    }
    pub fn tree(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let rel = entry
                    .path()
                    .strip_prefix(path)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                    out.push((rel, Vec::new()));
                } else {
                    out.push((rel, fs::read(entry.path()).unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}

/// The copy minus the two files the commit rewrites (EULA and port).
fn configured(tree: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    tree.into_iter()
        .filter(|(p, _)| p != "eula.txt" && p != "server.properties")
        .collect()
}
fn key(job: &ImportJob) -> String {
    format!("key-{}", job.review.as_ref().unwrap().plan_id)
}
fn applied(f: &Fixture, folder: &str) -> (ImportJob, ApplyAccepted) {
    let job = f.plan(folder);
    assert_eq!(
        job.state,
        "review",
        "{:?}",
        job.review.as_ref().map(|r| &r.blockers)
    );
    let accepted = f
        .jobs
        .apply(&f.binding, &Fixture::apply_request(&job), &key(&job))
        .unwrap();
    (job, accepted)
}

#[test]
fn vanilla_and_fabric_plan_apply_copy_preserving_source() {
    for (folder, loader, version, launch) in [
        (
            "vanilla",
            "vanilla",
            "1.21.1",
            "minecraft_server.1.21.1.jar",
        ),
        ("fabric", "fabric", "1.20.1", "fabric-server-launch.jar"),
    ] {
        let f = Fixture::new();
        f.vanilla();
        f.fabric();
        let before = Fixture::tree(&f.source(folder));
        let job = f.plan(folder);
        let review = job.review.clone().unwrap();
        assert_eq!(job.state, "review");
        assert_eq!(review.detected.loader.as_deref(), Some(loader));
        assert_eq!(review.detected.minecraft_version.as_deref(), Some(version));
        assert_eq!(
            review.detected.launch.as_ref().unwrap().relative_path,
            launch
        );
        assert_eq!(review.resources.game_port, Some(25_565));
        assert!(review.blockers.is_empty());
        assert!(review.source.snapshot.is_some());
        let accepted = f
            .jobs
            .apply(&f.binding, &Fixture::apply_request(&job), "key-1")
            .unwrap();
        assert_eq!(accepted.server_id, review.destination.server_id);
        let done = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
        assert_eq!(done.state, "ready", "{:?}", done.error);
        assert_eq!(done.progress.files_done, done.progress.files_total);
        assert_eq!(done.progress.bytes_done, review.source.bytes);
        let game = f.jobs.server_dir(accepted.server_id).join("game");
        assert_eq!(configured(Fixture::tree(&game)), configured(before.clone()));
        assert_eq!(Fixture::tree(&f.source(folder)), before);
        // The destination path is per-server, never a singleton.
        assert!(
            game.to_string_lossy()
                .contains(&accepted.server_id.to_string())
        );
    }
}

#[test]
fn blocked_plans_have_no_confirmation_and_apply_refuses_bad_input() {
    let f = Fixture::new();
    f.vanilla();
    f.env.running.store(true, Ordering::SeqCst);
    let job = f.plan("vanilla");
    assert_eq!(job.state, "blocked");
    let review = job.review.clone().unwrap();
    assert!(review.blockers.contains(&"source_running".into()));
    assert!(review.confirmation.is_none());
    let request = ApplyRequest {
        plan_id: review.plan_id,
        confirmation: "x".into(),
        acknowledged_warnings: review.warnings,
        eula_accepted: true,
        source_stopped_confirmed: true,
    };
    assert_eq!(
        f.jobs.apply(&f.binding, &request, "k").unwrap_err(),
        JobError::BLOCKED
    );

    f.env.running.store(false, Ordering::SeqCst);
    let job = f.plan("vanilla");
    let good = Fixture::apply_request(&job);
    for (request, error) in [
        (
            ApplyRequest {
                confirmation: "wrong".into(),
                ..good.clone()
            },
            JobError::CONFIRMATION,
        ),
        (
            ApplyRequest {
                eula_accepted: false,
                ..good.clone()
            },
            JobError::EULA,
        ),
        (
            ApplyRequest {
                source_stopped_confirmed: false,
                ..good.clone()
            },
            JobError::SOURCE_STOP,
        ),
        (
            ApplyRequest {
                acknowledged_warnings: vec![],
                ..good.clone()
            },
            JobError::WARNINGS,
        ),
    ] {
        assert_eq!(f.jobs.apply(&f.binding, &request, "k").unwrap_err(), error);
    }
    let stranger = OwnerBinding {
        account_id: Uuid::new_v4().to_string(),
        ..f.binding.clone()
    };
    assert_eq!(
        f.jobs.apply(&stranger, &good, "k").unwrap_err(),
        JobError::FORBIDDEN
    );
}

#[test]
fn duplicate_apply_is_idempotent_and_conflicting_body_is_refused() {
    let f = Fixture::new();
    f.vanilla();
    let (job, first) = applied(&f, "vanilla");
    let request = Fixture::apply_request(&job);
    let again = f.jobs.apply(&f.binding, &request, &key(&job)).unwrap();
    assert_eq!(again, first);
    let changed = ApplyRequest {
        acknowledged_warnings: vec![],
        ..request.clone()
    };
    assert_eq!(
        f.jobs.apply(&f.binding, &changed, &key(&job)).unwrap_err(),
        JobError::IDEMPOTENCY
    );
    assert_eq!(
        f.jobs.apply(&f.binding, &request, "key-2").unwrap_err(),
        JobError::CONFLICT
    );
    // A second plan cannot reserve past the one-server capacity.
    f.fabric();
    let other = f.plan("fabric");
    assert_eq!(
        f.jobs
            .apply(&f.binding, &Fixture::apply_request(&other), "key-3")
            .unwrap_err(),
        JobError("name_in_use")
    );
    f.jobs.run_apply(&f.binding, &first.job_id).unwrap();
    assert_eq!(
        f.jobs.apply(&f.binding, &request, &key(&job)).unwrap(),
        first
    );
}

#[test]
fn source_changed_after_review_blocks_and_requires_replan() {
    let f = Fixture::new();
    f.vanilla();
    let (_, accepted) = applied(&f, "vanilla");
    f.put("vanilla/world/level.dat", "changed while live");
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "blocked");
    assert_eq!(job.error.as_deref(), Some("source_changed"));
    assert!(!f.jobs.server_dir(accepted.server_id).join("game").exists());
    // Reservation released: a fresh plan can be applied.
    let (_, again) = applied(&f, "vanilla");
    assert_eq!(
        f.jobs.run_apply(&f.binding, &again.job_id).unwrap().state,
        "ready"
    );
}

#[test]
fn source_changed_mid_copy_quarantines_destination() {
    let f = Fixture::new();
    f.vanilla();
    let (_, accepted) = applied(&f, "vanilla");
    let target = f.source("vanilla").join("world/region/r.0.0.mca");
    f.env.set_hook(Some(Box::new(move |index| {
        if index == 2 {
            fs::write(&target, "a live world write").unwrap();
        }
    })));
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "blocked");
    assert_eq!(job.error.as_deref(), Some("source_changed"));
    let dir = f.jobs.server_dir(accepted.server_id);
    assert!(!dir.join("game").exists() && !dir.join("staging").exists());
    assert!(fs::read_dir(&dir).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("quarantine-")
    }));
}

#[test]
fn port_taken_or_source_started_before_copy_blocks() {
    let f = Fixture::new();
    f.vanilla();
    let (_, accepted) = applied(&f, "vanilla");
    *f.env.busy_port.lock().unwrap() = Some(25_565);
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.error.as_deref(), Some("game_port_in_use"));
    *f.env.busy_port.lock().unwrap() = None;
    let (_, accepted) = applied(&f, "vanilla");
    f.env.running.store(true, Ordering::SeqCst);
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.error.as_deref(), Some("source_running"));
}

#[test]
fn low_disk_at_plan_and_mid_copy() {
    let f = Fixture::new();
    f.vanilla();
    f.env.free.store(1024, Ordering::SeqCst);
    let job = f.plan("vanilla");
    assert!(
        job.review
            .unwrap()
            .blockers
            .contains(&"insufficient_space".into())
    );
    f.env.free.store(100 * GIB, Ordering::SeqCst);
    let (_, accepted) = applied(&f, "vanilla");
    // Plenty at the pre-copy check, then the disk fills during the copy.
    let calls = f.env.free_calls.load(Ordering::SeqCst);
    f.env.low_after.store(calls + 3, Ordering::SeqCst);
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "blocked");
    assert_eq!(job.error.as_deref(), Some("insufficient_space"));
    assert!(!f.jobs.server_dir(accepted.server_id).exists());
    assert!(f.source("vanilla").join("world/level.dat").exists());
}

#[test]
fn cancel_mid_copy_cleans_destination_and_keeps_source() {
    let f = Fixture::new();
    f.vanilla();
    let (_, accepted) = applied(&f, "vanilla");
    let jobs = f.jobs.clone();
    let binding = f.binding.clone();
    let id = accepted.job_id.clone();
    f.env.set_hook(Some(Box::new(move |index| {
        if index == 3 {
            assert_eq!(jobs.cancel(&binding, &id).unwrap().state, "applying");
        }
    })));
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "cancelled");
    assert!(!f.jobs.server_dir(accepted.server_id).exists());
    assert_eq!(Fixture::tree(&f.source("vanilla")).len(), 7);
    assert_eq!(
        f.jobs.cancel(&f.binding, &accepted.job_id).unwrap().state,
        "cancelled"
    );
}

#[test]
fn crash_mid_copy_resumes_without_recopying_or_duplicating() {
    let mut f = Fixture::new();
    f.vanilla();
    let before = Fixture::tree(&f.source("vanilla"));
    let (_, accepted) = applied(&f, "vanilla");
    f.env.set_hook(Some(Box::new(|index| {
        if index == 4 {
            panic!("simulated crash");
        }
    })));
    let jobs = f.jobs.clone();
    let binding = f.binding.clone();
    let id = accepted.job_id.clone();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        jobs.run_apply(&binding, &id)
    }));
    assert!(crashed.is_err());
    f.env.set_hook(None);
    // A stray part file from the torn write must not survive the resume.
    let staging = f.jobs.server_dir(accepted.server_id).join("staging");
    fs::write(staging.join(".motd.vesta-part"), "torn").unwrap();
    f.reopen();
    let job = f.jobs.status(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "interrupted");
    assert_eq!(job.phase, "copy");
    assert_eq!(
        f.jobs.resume(&f.binding, &accepted.job_id).unwrap().state,
        "applying"
    );
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "ready", "{:?}", job.error);
    let game = f.jobs.server_dir(accepted.server_id).join("game");
    assert_eq!(configured(Fixture::tree(&game)), configured(before));
}

#[test]
fn crash_resume_with_clean_staging_completes() {
    let mut f = Fixture::new();
    f.fabric();
    let before = Fixture::tree(&f.source("fabric"));
    let (_, accepted) = applied(&f, "fabric");
    f.env.set_hook(Some(Box::new(|index| {
        if index == 5 {
            panic!("simulated crash");
        }
    })));
    let jobs = f.jobs.clone();
    let binding = f.binding.clone();
    let id = accepted.job_id.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || jobs.run_apply(&binding, &id)
        ))
        .is_err()
    );
    f.env.set_hook(None);
    f.reopen();
    f.jobs.resume(&f.binding, &accepted.job_id).unwrap();
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "ready", "{:?}", job.error);
    let game = f.jobs.server_dir(accepted.server_id).join("game");
    assert_eq!(configured(Fixture::tree(&game)), configured(before));
    // Interrupted then cancelled jobs clean up their unregistered copy.
    let f2 = Fixture::new();
    f2.vanilla();
    let (_, accepted) = applied(&f2, "vanilla");
    assert_eq!(
        f2.jobs.cancel(&f2.binding, &accepted.job_id).unwrap().state,
        "applying"
    );
    assert_eq!(
        f2.jobs
            .run_apply(&f2.binding, &accepted.job_id)
            .unwrap()
            .state,
        "cancelled"
    );
}

#[path = "commit_tests.rs"]
mod commit_tests;

#[test]
fn unknown_process_requires_owner_acknowledgement() {
    let f = Fixture::new();
    f.vanilla();
    f.env.unknown.store(true, Ordering::SeqCst);
    let job = f.plan("vanilla");
    assert!(job.review.as_ref().unwrap().blockers.is_empty());
    assert!(
        job.review
            .as_ref()
            .unwrap()
            .warnings
            .contains(&"source_process_unknown".to_owned())
    );
    let mut request = Fixture::apply_request(&job);
    request.source_stopped_confirmed = false;
    assert_eq!(
        f.jobs.apply(&f.binding, &request, "unknown").unwrap_err(),
        JobError::SOURCE_STOP
    );
    request.source_stopped_confirmed = true;
    request
        .acknowledged_warnings
        .retain(|v| v != "source_process_unknown");
    assert_eq!(
        f.jobs.apply(&f.binding, &request, "unknown").unwrap_err(),
        JobError::WARNINGS
    );
}
#[test]
fn escaping_world_blocks_and_bind_address_warns() {
    let f = Fixture::new();
    f.vanilla();
    f.put(
        "vanilla/server.properties",
        "level-name=../outside\nserver-ip=192.0.2.1\n",
    );
    let job = f.plan("vanilla");
    let r = job.review.unwrap();
    assert!(r.blockers.contains(&"world_outside_source".into()));
    assert!(r.warnings.contains(&"bind_address_cleared".into()));
}

#[test]
fn non_server_plan_has_only_cause_and_suggests_child() {
    let f = Fixture::new();
    f.vanilla();
    let job = f.plan("");
    let review = job.review.unwrap();
    assert_eq!(review.blockers, vec!["not_minecraft_server"]);
    assert!(review.warnings.is_empty());
    assert_eq!(review.nearby.len(), 1);
    assert!(review.nearby[0].path.ends_with("vanilla"));
    assert!(
        f.browser
            .open(&f.binding, &review.nearby[0].folder_token)
            .is_ok()
    );
}

#[test]
fn two_servers_reserve_distinct_ports_and_shared_memory() {
    let mut f = Fixture::new();
    f.vanilla();
    f.fabric();
    let first = f.plan("vanilla");
    let a = f
        .jobs
        .apply(&f.binding, &Fixture::apply_request(&first), "first")
        .unwrap();
    f.jobs.run_apply(&f.binding, &a.job_id).unwrap();
    let second = f
        .jobs
        .plan(
            &f.binding,
            PlanRequest {
                folder_token: f.token("fabric"),
                name: "Creative".into(),
                game_port: None,
                memory_mib: Some(2048),
                launch_choice: None,
                minecraft_version: None,
            },
        )
        .unwrap();
    assert_eq!(
        second.review.as_ref().unwrap().resources.game_port,
        Some(25566)
    );
    let b = f
        .jobs
        .apply(&f.binding, &Fixture::apply_request(&second), "second")
        .unwrap();
    f.jobs.run_apply(&f.binding, &b.job_id).unwrap();
    assert!(f.state.server(a.server_id).unwrap().is_some());
    assert!(f.state.server(b.server_id).unwrap().is_some());
    let held = f.jobs.read(|c| reservations_in(c, None)).unwrap();
    assert_eq!(held.memory_mib, 4096);
    let (memory, port) = f.jobs.resource_plans(None, None).unwrap();
    assert_eq!(memory.reserved_mib, 4096);
    assert_eq!(port.game_port, Some(25567));
    assert_eq!(
        resources::plan_memory(Some(16 * GIB), &held, Some(16 * 1024)).blocker,
        Some("memory_budget_exceeded")
    );
    f.reopen();
    let first_worker = f
        .jobs
        .create_worker(a.server_id, Arc::new(SystemClock))
        .unwrap();
    let second_worker = f
        .jobs
        .create_worker(b.server_id, Arc::new(SystemClock))
        .unwrap();
    assert_ne!(
        first_worker.handle().server_id(),
        second_worker.handle().server_id()
    );
    first_worker
        .shutdown(std::time::Duration::from_secs(10))
        .unwrap();
    second_worker
        .shutdown(std::time::Duration::from_secs(10))
        .unwrap();
}

#[cfg(unix)]
#[test]
fn unreadable_source_plan_reports_only_the_source_cause() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.vanilla();
    let source = f.source("vanilla");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = fs::read_dir(&source).is_err();
    let job = if enforced {
        Some(f.plan("vanilla"))
    } else {
        None
    };
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    if let Some(job) = job {
        let review = job.review.unwrap();
        assert_eq!(review.blockers, vec!["source_unreadable"]);
        assert!(review.warnings.is_empty());
    }
}

#[test]
fn create_defaults_include_pending_import_name_port_and_memory_reservations() {
    let f = Fixture::new();
    f.vanilla();
    let job = f.jobs.plan(&f.binding, PlanRequest {
        folder_token: f.token("vanilla"), name: "minecraft server".into(), game_port: None,
        memory_mib: Some(8192), launch_choice: None, minecraft_version: None,
    }).unwrap();
    let accepted = f.jobs.apply(&f.binding, &Fixture::apply_request(&job), "reserved-default").unwrap();
    assert_eq!(f.jobs.suggested_create_name().unwrap(), "Minecraft Server 2");
    let (memory, port) = f.jobs.resource_plans(None, None).unwrap();
    assert_eq!(port.game_port, Some(25566));
    assert_eq!(memory.memory_mib, Some(4096));
    assert_eq!(memory.reserved_mib, 8192);
    f.jobs.cancel(&f.binding, &accepted.job_id).unwrap();
    f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(f.jobs.suggested_create_name().unwrap(), "Minecraft Server");
    let (memory, port) = f.jobs.resource_plans(None, None).unwrap();
    assert_eq!(port.game_port, Some(25565));
    assert_eq!(memory.reserved_mib, 0);
}

/// Startup removes what settled, never-committed jobs left on disk, and
/// nothing else: not interrupted jobs (resume needs them), not registered
/// servers, not folders no job knows.
#[test]
fn startup_sweeps_destinations_of_abandoned_jobs_only() {
    let mut f = Fixture::new();
    let servers = f.temp.path().join("servers");
    let mut ids = Vec::new();
    for (index, (kind, state)) in [
        ("create", "failed"),
        ("create", "cancelled"),
        ("import", "failed"),
        ("create", "interrupted"),
        ("create", "ready"),
    ]
    .into_iter()
    .enumerate()
    {
        let server = Uuid::new_v4();
        fs::create_dir_all(servers.join(server.to_string()).join("staging/world")).unwrap();
        fs::create_dir_all(servers.join(server.to_string()).join("install")).unwrap();
        f.state
            .write(|c| {
                c.execute(
                    "INSERT INTO machine_import_jobs(job_id,account_id,device_id,state,phase,request,server_id,kind,created_ms,updated_ms) VALUES(?1,'a','d',?2,'resolving','{}',?3,?4,0,0)",
                    params![format!("job-{index}"), state, server.to_string(), kind],
                )?;
                Ok(())
            })
            .unwrap();
        ids.push(server);
    }
    let unknown = servers.join(Uuid::new_v4().to_string());
    fs::create_dir_all(&unknown).unwrap();
    f.reopen();
    let dir = |i: usize| servers.join(ids[i].to_string());
    assert!(!dir(0).exists() && !dir(1).exists(), "abandoned creates removed");
    // The import's staging copy goes; anything else it holds stays.
    assert!(!dir(2).join("staging").exists() && dir(2).join("install").exists());
    assert!(dir(3).join("staging").exists(), "interrupted kept for resume");
    assert!(dir(4).exists(), "a committed job keeps its server");
    assert!(unknown.exists(), "unknown folders are never touched");
}
