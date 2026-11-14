//! Import jobs through the host router: owner-only plan/apply/status/cancel
//! with a tiny fake fixture, and one ignored real end-to-end import.
use super::*;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use vesta_control::{
    java::HostJavaPlan,
    machine::imports::{
        ImportRootConfig,
        inspection::ProcessEvidence,
        jobs::{HostEnvironment, ImportEnvironment, ReviewRuntime},
    },
};

const GIB: u64 = 1024 * 1024 * 1024;

struct FakeCatalogue;
impl vesta_control::machine::creates::Catalogue for FakeCatalogue {
    fn minecraft(&self) -> Result<Vec<vesta_server_install::MinecraftVersion>, JobError> {
        Ok(vec![
            vesta_server_install::MinecraftVersion {
                id: "26.2-snapshot-1".into(),
                stable: false,
                experimental: true,
                recommended: false,
            },
            vesta_server_install::MinecraftVersion {
                id: "26.1".into(),
                stable: true,
                experimental: false,
                recommended: true,
            },
        ])
    }
    fn releases(
        &self,
        kind: vesta_server_install::Software,
        _: &str,
    ) -> Result<Vec<vesta_server_install::SoftwareRelease>, JobError> {
        if kind == vesta_server_install::Software::Spigot {
            return Err(JobError("provider_unavailable"));
        }
        if kind == vesta_server_install::Software::Bukkit {
            return Ok(Vec::new());
        }
        Ok(vec![vesta_server_install::SoftwareRelease {
            id: "42".into(),
            build: Some(42),
            loader_version: None,
        }])
    }
    fn source_build_tools(&self) -> bool {
        true
    }
}
/// A machine without git: CraftBukkit and Spigot cannot be built.
struct NoGitCatalogue;
impl vesta_control::machine::creates::Catalogue for NoGitCatalogue {
    fn minecraft(&self) -> Result<Vec<vesta_server_install::MinecraftVersion>, JobError> {
        FakeCatalogue.minecraft()
    }
    fn releases(
        &self,
        kind: vesta_server_install::Software,
        _: &str,
    ) -> Result<Vec<vesta_server_install::SoftwareRelease>, JobError> {
        assert!(
            !matches!(
                kind,
                vesta_server_install::Software::Bukkit | vesta_server_install::Software::Spigot
            ),
            "providers are not asked for software this machine cannot build"
        );
        FakeCatalogue.releases(kind, "26.1")
    }
    fn source_build_tools(&self) -> bool {
        false
    }
}

#[test]
fn catalogue_marks_source_built_kinds_requires_git_without_git() {
    let host = Host::start(
        tempfile::tempdir().unwrap(),
        Arc::new(FakeEnv::default()),
        None,
        None,
        Some(Arc::new(NoGitCatalogue)),
    );
    let (status, value) = host.call(
        "GET",
        "/v1/host/catalogue/software?minecraftVersion=26.1",
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{value}");
    for kind in value["kinds"].as_array().unwrap() {
        if kind["kind"] == "bukkit" || kind["kind"] == "spigot" {
            assert_eq!(kind["available"], false, "{kind}");
            assert_eq!(kind["unavailableReason"], "requires_git", "{kind}");
            assert_eq!(kind["builds"], json!([]));
        } else {
            assert_ne!(kind["unavailableReason"], "requires_git", "{kind}");
        }
    }
}

/// Deterministic observations: source stopped, ports free, plenty of disk.
#[derive(Default)]
struct FakeEnv {
    /// Milliseconds to pause before each copied entry (slow copy for cancel).
    delay_ms: AtomicU64,
    java: PathBuf,
    /// While set, making Java available waits (restore cancel/restart tests).
    java_gate: std::sync::atomic::AtomicBool,
}
impl ImportEnvironment for FakeEnv {
    fn processes(&self, _: &[PathBuf]) -> (Vec<ProcessEvidence>, &'static str) {
        (Vec::new(), "complete")
    }
    fn port_state(&self, port: u16) -> &'static str {
        if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() { "free" } else { "occupied" }
    }
    fn free_bytes(&self, _: &Path) -> Option<u64> {
        Some(100 * GIB)
    }
    fn memory_total_bytes(&self) -> Option<u64> {
        Some(16 * GIB)
    }
    fn plan_java(&self, version: Option<&str>, _: &[PathBuf]) -> HostJavaPlan {
        vesta_control::java::plan_from(&[], version)
    }
    fn ensure_java(&self, _: &ReviewRuntime, _: &[PathBuf]) -> Result<PathBuf, &'static str> {
        while self.java_gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(self.java.clone())
    }
    fn before_copy_entry(&self, _: u64) {
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
    }
}

/// Real host observations except process correlation, which only Linux can
/// observe; the fixture source is created by the test and never running.
struct WindowsSafeEnv(HostEnvironment);
impl ImportEnvironment for WindowsSafeEnv {
    fn processes(&self, sources: &[PathBuf]) -> (Vec<ProcessEvidence>, &'static str) {
        if cfg!(target_os = "linux") {
            self.0.processes(sources)
        } else {
            (Vec::new(), "complete")
        }
    }
    fn plan_java(&self, version: Option<&str>, excluded: &[PathBuf]) -> HostJavaPlan {
        self.0.plan_java(version, excluded)
    }
    fn ensure_java(
        &self,
        runtime: &ReviewRuntime,
        excluded: &[PathBuf],
    ) -> Result<PathBuf, &'static str> {
        self.0.ensure_java(runtime, excluded)
    }
}

struct Host {
    temp: tempfile::TempDir,
    state: HostState,
    router: HostRouter,
    transport: Option<Transport>,
    owner: String,
    stranger: String,
    /// Owner and stranger device ids, reused when the host restarts.
    devices: (Uuid, Uuid),
}
impl Host {
    fn new(env: Arc<dyn ImportEnvironment>) -> Self {
        Self::start(tempfile::tempdir().unwrap(), env, None, None, None)
    }
    /// Build (or, with `devices`, rebuild after a restart) a host on `temp`.
    fn start(
        temp: tempfile::TempDir,
        env: Arc<dyn ImportEnvironment>,
        creator: Option<Arc<dyn vesta_control::machine::creates::CreateInstaller>>,
        devices: Option<(Uuid, Uuid)>,
        catalogue: Option<Arc<dyn vesta_control::machine::creates::Catalogue>>,
    ) -> Self {
        let base = temp.path();
        for dir in ["host-state", "imports", "servers"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        let state =
            HostState::open(base.join("host-state"), Arc::new(vesta_state::SystemClock)).unwrap();
        let host = state.snapshot().unwrap().host_id;
        let key = Ed25519KeyPair::from_seed_unchecked(&[71; 32]).unwrap();
        let stranger_key = Ed25519KeyPair::from_seed_unchecked(&[72; 32]).unwrap();
        let (device, stranger) = devices.unwrap_or_else(|| {
            let device = state
                .claim(
                    Uuid::new_v4(),
                    "Owner",
                    key.public_key().as_ref().try_into().unwrap(),
                )
                .unwrap();
            let stranger = state
                .write(|c| {
                    HostState::enroll_device_in(
                        c,
                        Uuid::new_v4(),
                        "Stranger",
                        stranger_key.public_key().as_ref().try_into().unwrap(),
                        state.now_ms(),
                    )
                })
                .unwrap();
            (device.device_id, stranger.device_id)
        });
        let identity = Arc::new(HostIdentity::new(state.clone()).unwrap());
        std::fs::write(
            base.join("cert.pem"),
            include_bytes!("../../tests/fixtures/test-only-server.pem"),
        )
        .unwrap();
        std::fs::write(
            base.join("key.pem"),
            include_bytes!("../../tests/fixtures/test-only-server-key.pem"),
        )
        .unwrap();
        let listener = TlsListener::host(
            "127.0.0.1".parse().unwrap(),
            25500,
            base.join("cert.pem"),
            base.join("key.pem"),
        )
        .unwrap()
        .on_ephemeral_port();
        let folders = Arc::new(
            FolderBrowser::new(
                vec![ImportRootConfig {
                    root_id: "r1".into(),
                    label: "Imports".into(),
                    path: base.join("imports"),
                }],
                vec![base.join("host-state"), base.join("servers")],
            )
            .unwrap(),
        );
        let jobs =
            ImportJobs::open(state.clone(), folders.clone(), &base.join("servers"), env).unwrap();
        let factory = jobs.clone();
        let mut router = HostRouter::new(identity, "Test machine".into(), folders)
            .with_imports(jobs, move |id| {
                factory.create_worker(id, Arc::new(vesta_state::SystemClock))
            })
            .with_create_catalogue(catalogue.unwrap_or_else(|| Arc::new(FakeCatalogue)));
        if let Some(creator) = creator {
            router = router.with_create_installer(creator);
        }
        let devices = (device, stranger);
        let transport = Transport::start_host(router.clone(), &listener).unwrap();
        let mut wire = Wire::connect(transport.local_addr());
        let owner = format!(
            "Bearer {}",
            session(&mut wire, host, device, &key)
        );
        let stranger = format!(
            "Bearer {}",
            session(&mut wire, host, stranger, &stranger_key)
        );
        Self {
            temp,
            state,
            router,
            transport: Some(transport),
            owner,
            stranger,
            devices,
        }
    }
    /// Stop this host (transport and workers, like a service stop) and start
    /// a new process on the same state, servers and identity.
    fn restart(
        mut self,
        env: Arc<dyn ImportEnvironment>,
        creator: Option<Arc<dyn vesta_control::machine::creates::CreateInstaller>>,
    ) -> Self {
        let temp = std::mem::replace(&mut self.temp, tempfile::tempdir().unwrap());
        let devices = self.devices;
        drop(self);
        for entry in std::fs::read_dir(temp.path().join("servers")).unwrap().flatten() { let _ = std::fs::remove_file(entry.path().join("game/fixture-exit")); }
        Self::start(temp, env, creator, Some(devices), None)
    }
    fn wire(&self) -> Wire {
        Wire::connect(self.transport.as_ref().unwrap().local_addr())
    }
    fn put(&self, relative: &str, bytes: &[u8]) {
        let path = self.temp.path().join("imports").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    fn call(&self, method: &str, path: &str, body: Value, auth: &str) -> (u16, Value) {
        self.wire().request(method, path, body, Some(auth))
    }
    fn token(&self, folder: &str) -> String {
        let (status, r) = self.call(
            "POST",
            "/v1/host/folders/resolve",
            json!({"rootId":"r1","relativePath":folder}),
            &self.owner,
        );
        assert_eq!(status, 200, "{r}");
        r["folderToken"].as_str().unwrap().to_owned()
    }
    /// Poll the job until `done` holds or the deadline passes.
    fn wait(&self, job: &str, timeout: Duration, done: impl Fn(&Value) -> bool) -> Value {
        let until = std::time::Instant::now() + timeout;
        loop {
            let (status, value) = self.call(
                "GET",
                &format!("/v1/host/imports/jobs/{job}"),
                Value::Null,
                &self.owner,
            );
            // Polling shares the per-peer request budget; back off when told.
            if status == 429 {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            assert_eq!(status, 200, "{value}");
            if done(&value) {
                return value;
            }
            assert!(
                std::time::Instant::now() < until,
                "job did not settle: {value}"
            );
            std::thread::sleep(if timeout > Duration::from_secs(60) {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(150)
            });
        }
    }
    fn plan(&self, folder: &str, extra: Value) -> Value {
        let mut body = json!({"folderToken":self.token(folder),"name":"Survival","gamePort":null,"memoryMiB":1024,"launchChoice":null,"minecraftVersion":null});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (status, accepted) = self.call("POST", "/v1/host/imports/plan", body, &self.owner);
        assert_eq!(status, 202, "{accepted}");
        assert_eq!(accepted["state"], "planning");
        let id = accepted["jobId"].as_str().unwrap().to_owned();
        self.wait(&id, Duration::from_secs(60), |j| j["state"] != "planning")
    }
    fn apply(&self, job: &Value, key: &str) -> (u16, Value) {
        let review = &job["review"];
        self.call(
            "POST",
            "/v1/host/imports/apply",
            json!({"planId":review["planId"],"confirmation":review["confirmation"],"acknowledgedWarnings":review["warnings"],"eulaAccepted":true,"sourceStoppedConfirmed":true}),
            &format!("{}\r\nIdempotency-Key: {key}", self.owner),
        )
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        // Model the service cgroup ending owned fixture games before worker shutdown.
        if let Some(jobs) = self.router.imports() {
            if let Ok(entries) = std::fs::read_dir(jobs.servers_root()) {
                for entry in entries.flatten() { let game=entry.path().join("game"); if game.join("vesta-create-fixture").is_file() { let _=std::fs::write(game.join("fixture-exit"),b"exit"); } }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        if let Some(t) = self.transport.take() {
            let _ = t.shutdown(Duration::from_secs(5));
        }
        for w in self.router.take_owned_workers() {
            let _ = w.shutdown(Duration::from_secs(30));
        }
    }
}
fn fake_vanilla(host: &Host) {
    host.put(
        "vanilla/server.properties",
        format!("server-port={}\nmotd=hi\nenable-rcon=true\nrcon.password=hunter2\n", free_port()).as_bytes(),
    );
    host.put("vanilla/minecraft_server.1.21.1.jar", b"jar");
    host.put("vanilla/world/level.dat", b"nbt");
    host.put("vanilla/world/region/r.0.0.mca", b"region");
    std::fs::File::options()
        .write(true)
        .open(
            host.temp
                .path()
                .join("imports/vanilla/world/region/r.0.0.mca"),
        )
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(120)),
        )
        .unwrap();
    host.put("vanilla/eula.txt", b"eula=false\n");
}
fn fake_env() -> Arc<FakeEnv> {
    Arc::new(FakeEnv {
        java: std::env::temp_dir().join("vesta-test-java-not-run"),
        ..FakeEnv::default()
    })
}

#[test]
fn owner_plans_applies_and_polls_import_jobs_through_the_router() {
    let host = Host::new(fake_env());
    fake_vanilla(&host);
    let (_, info) = host.call("GET", "/v1/host", Value::Null, &host.owner);
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("import-jobs"))
    );
    // Non-owners cannot plan, read, apply, cancel or start.
    let token = host.token("vanilla");
    let body = json!({"folderToken":token,"name":"x","gamePort":null,"memoryMiB":null,"launchChoice":null,"minecraftVersion":null});
    assert_eq!(
        host.call("POST", "/v1/host/imports/plan", body, &host.stranger)
            .0,
        403
    );
    let job = host.plan("vanilla", json!({}));
    assert_eq!(job["state"], "review", "{job}");
    let id = job["jobId"].as_str().unwrap();
    let review = &job["review"];
    assert!(review["confirmation"].is_string());
    assert!(review["expiresAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(review["resources"]["memoryMiB"], 1024);
    assert_eq!(review["detected"]["minecraftVersion"], "1.21.1");
    assert_eq!(
        review["warnings"],
        json!(["eula_not_inherited", "rcon_disabled"])
    );
    assert!(!job.to_string().contains("hunter2"));
    for (method, path) in [
        ("GET", format!("/v1/host/imports/jobs/{id}")),
        ("POST", format!("/v1/host/imports/jobs/{id}/cancel")),
        ("POST", format!("/v1/host/imports/jobs/{id}/start")),
        ("POST", "/v1/host/imports/apply".into()),
    ] {
        let (status, _) = host.call(method, &path, Value::Null, &host.stranger);
        assert_eq!(status, 403, "{method} {path}");
    }
    assert_eq!(
        host.call(
            "GET",
            &format!("/v1/host/imports/jobs/{}", Uuid::new_v4()),
            Value::Null,
            &host.owner
        )
        .0,
        404
    );
    // Apply needs an Idempotency-Key, explicit EULA and acknowledged warnings.
    let (status, error) = host.call(
        "POST",
        "/v1/host/imports/apply",
        json!({"planId":review["planId"],"confirmation":review["confirmation"],"acknowledgedWarnings":review["warnings"],"eulaAccepted":true,"sourceStoppedConfirmed":true}),
        &host.owner,
    );
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("idempotency_key_required"))
    );
    let mut unacknowledged = job.clone();
    unacknowledged["review"]["warnings"] = json!([]);
    let (status, error) = host.apply(&unacknowledged, "apply-key-0");
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("warnings_unacknowledged"))
    );
    let (status, accepted) = host.apply(&job, "apply-key-1");
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(host.apply(&job, "apply-key-1").1, accepted);
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    let done = host.wait(id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting")) && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(done["state"], "ready", "{done}");
    assert_eq!(done["error"], "start_failed");
    assert_eq!(done["phase"], "start");
    assert_eq!(done["serverId"], server);
    assert!(done["review"]["confirmation"].is_null());
    let progress = &done["progress"];
    assert_eq!(progress["filesDone"], progress["filesTotal"]);
    assert_eq!(progress["bytesDone"], progress["bytesTotal"]);
    assert_eq!(progress["filesTotal"], 5);
    let game = host.temp.path().join("servers").join(&server).join("game");
    let properties = std::fs::read_to_string(game.join("server.properties")).unwrap();
    assert!(properties.contains("enable-rcon=false") && !properties.contains("enable-rcon=true"));
    // The registered server is listed for the owner and not for the stranger.
    let (_, list) = host.call("GET", "/v1/host/servers", Value::Null, &host.owner);
    assert_eq!(list["servers"][0]["serverId"], server);
    assert_eq!(list["servers"][0]["state"], "registered");
    // Imported Vanilla's own version is its Minecraft version.
    assert_eq!(
        list["servers"][0]["software"],
        json!({"kind":"vanilla","version":"1.21.1"})
    );
    assert_eq!(list["servers"][0]["minecraftVersion"], "1.21.1");
    assert_eq!(
        host.call("GET", "/v1/host/servers", Value::Null, &host.stranger)
            .1["servers"],
        json!([])
    );
    // A finished job cannot be cancelled; the registry still holds one server.
    let (status, error) = host.call(
        "POST",
        &format!("/v1/host/imports/jobs/{id}/cancel"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (409, json!("conflict"))
    );
    // Resume is only for interrupted jobs.
    assert_eq!(
        host.call(
            "POST",
            &format!("/v1/host/imports/jobs/{id}/resume"),
            Value::Null,
            &host.owner
        )
        .0,
        409
    );
    // The source was never written.
    assert_eq!(
        std::fs::read(host.temp.path().join("imports/vanilla/eula.txt")).unwrap(),
        b"eula=false\n"
    );
}

#[test]
fn owner_cancels_a_review_and_a_running_copy() {
    let env = fake_env();
    let host = Host::new(env.clone());
    fake_vanilla(&host);
    for i in 0..20 {
        host.put(&format!("vanilla/world/data/chunk-{i}.dat"), b"data");
    }
    let job = host.plan("vanilla", json!({}));
    let id = job["jobId"].as_str().unwrap();
    let (status, cancelled) = host.call(
        "POST",
        &format!("/v1/host/imports/jobs/{id}/cancel"),
        Value::Null,
        &host.owner,
    );
    assert_eq!((status, cancelled), (200, json!({"state":"cancelled"})));
    // A cancelled plan's confirmation can no longer be applied.
    assert_eq!(host.apply(&job, "cancelled-key").0, 409);

    env.delay_ms.store(150, Ordering::SeqCst);
    let job = host.plan("vanilla", json!({}));
    let id = job["jobId"].as_str().unwrap();
    let (status, accepted) = host.apply(&job, "copy-key");
    assert_eq!(status, 202, "{accepted}");
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    host.wait(id, Duration::from_secs(30), |j| j["phase"] == "copy");
    let (status, requested) = host.call(
        "POST",
        &format!("/v1/host/imports/jobs/{id}/cancel"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{requested}");
    assert_eq!(requested["state"], "applying"); // safe boundary pending
    let done = host.wait(id, Duration::from_secs(30), |j| !matches!(j["state"].as_str(), Some("applying" | "starting")) && (j["state"] != "ready" || !j["error"].is_null()));
    assert_eq!(done["state"], "cancelled", "{done}");
    assert!(!host.temp.path().join("servers").join(&server).exists());
    assert_eq!(
        host.call("GET", "/v1/host/servers", Value::Null, &host.owner)
            .1["servers"],
        json!([])
    );
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("0.0.0.0", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Real import: downloads the checksum-verified managed Java 21 runtime and a
/// genuine vanilla 1.21.1 server jar through the verified installer, imports
/// it through the router and runs it until online.
#[test]
#[ignore = "downloads Java and the vanilla server jar, then runs the server"]
fn real_vanilla_import_reaches_online_and_is_managed_through_mapped_routes() {
    let scratch = tempfile::tempdir().unwrap();
    let java_cache = scratch.path().join("java");
    let install = scratch.path().join("install");
    std::fs::create_dir_all(&java_cache).unwrap();
    std::fs::create_dir_all(&install).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let java = runtime
        .block_on(vesta_control::java::download_into(java_cache.clone(), 21))
        .expect("managed Java 21 download");
    let jar = runtime.block_on(async {
        use vesta_server_install::*;
        let installer = ServerInstaller::new(
            InstallRoots::new(&install, &[]).unwrap(),
            TransportConfig::direct(),
        )
        .unwrap();
        let selection = Selection {
            version: Some("1.21.1".into()),
            loader_version: None,
            build: None,
            experimental: false,
        };
        let plan = installer
            .plan(
                Software::Vanilla,
                &selection,
                Path::new(&java.path),
                &SilentProgressReporter,
            )
            .await
            .expect("vanilla plan");
        let approval = Approval::review(
            &plan,
            Acknowledgments {
                legacy_checksum: true,
                warnings: true,
            },
        )
        .unwrap();
        match installer
            .install(&plan, approval, &SilentProgressReporter)
            .await
            .expect("vanilla install")
        {
            InstallOutcome::Installed(_) => {}
            InstallOutcome::DryRun => panic!("dry run"),
        }
        plan.destination().join("software/vanilla.jar")
    });

    let env = Arc::new(WindowsSafeEnv(HostEnvironment {
        java_cache: java_cache.clone(),
    }));
    let host = Host::new(env);
    let port = free_port();
    host.put(
        "vanilla/server.properties",
        format!("server-port={port}\nonline-mode=false\nview-distance=2\nsimulation-distance=2\nenable-rcon=true\nrcon.password=hunter2\n").as_bytes(),
    );
    host.put(
        "vanilla/minecraft_server.1.21.1.jar",
        &std::fs::read(&jar).unwrap(),
    );
    host.put("vanilla/eula.txt", b"eula=false\n");

    // Browse to the fixture, inspect, plan.
    let (status, roots) = host.call("GET", "/v1/host/folders/roots", Value::Null, &host.owner);
    assert_eq!(status, 200, "{roots}");
    let (status, root) = host.call(
        "POST",
        "/v1/host/folders/resolve",
        json!({"rootId":"r1","relativePath":""}),
        &host.owner,
    );
    assert_eq!(status, 200, "{root}");
    let (status, listing) = host.call(
        "POST",
        "/v1/host/folders/list",
        json!({"folderToken":root["folderToken"],"cursor":null}),
        &host.owner,
    );
    assert_eq!(status, 200, "{listing}");
    let entry = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "vanilla")
        .expect("fixture listed")
        .clone();
    let (status, inspected) = host.call(
        "POST",
        "/v1/host/imports/inspect",
        json!({"folderToken":entry["folderToken"],"gamePort":port}),
        &host.owner,
    );
    assert_eq!(status, 200, "{inspected}");
    assert!(inspected["minecraft"].as_bool().unwrap(), "{inspected}");
    let job = host.plan(
        "vanilla",
        json!({"gamePort":port,"minecraftVersion":"1.21.1"}),
    );
    assert_eq!(job["state"], "review", "{job}");
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.apply(&job, "real-apply");
    assert_eq!(status, 202, "{accepted}");
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    let ready = host.wait(&id, Duration::from_secs(300), |j| !matches!(j["state"].as_str(), Some("applying" | "starting")) && (j["state"] != "ready" || !j["error"].is_null()));
    assert_eq!(ready["state"], "online", "{ready}");

    // Start: the HTTP call returns at once; readiness is polled.
    let (status, started) = host.call(
        "POST",
        &format!("/v1/host/imports/jobs/{id}/start"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 202, "{started}");
    assert_eq!(started["state"], "online", "{started}");
    let online = host.wait(&id, Duration::from_secs(600), |j| j["state"] != "starting");
    if online["state"] != "online" {
        // Diagnostics: the game console and managed status through the
        // mapped routes, plus the managed launch definition.
        for suffix in ["manage/console?limit=200", "manage/server/status"] {
            let (s, v) = host.call(
                "GET",
                &format!("/v1/servers/{server}/{suffix}"),
                Value::Null,
                &host.owner,
            );
            eprintln!("{suffix}: {s} {v:#}");
        }
        let dir = host.temp.path().join("servers").join(&server);
        eprintln!(
            "launch.toml:\n{}",
            std::fs::read_to_string(dir.join("private/launch.toml")).unwrap_or_default()
        );
        for log in ["game/logs/latest.log"] {
            eprintln!(
                "{log}:\n{}",
                std::fs::read_to_string(dir.join(log)).unwrap_or_default()
            );
        }
    }
    assert_eq!(online["state"], "online", "{online}");
    let (_, list) = host.call("GET", "/v1/host/servers", Value::Null, &host.owner);
    assert_eq!(list["servers"][0]["serverId"], server);
    assert_eq!(list["servers"][0]["state"], "online", "{list}");

    // Mapped management routes work for the imported server without restart.
    let status_path = format!("/v1/servers/{server}/manage/server/status");
    let (status, view) = host.call("GET", &status_path, Value::Null, &host.owner);
    assert_eq!(status, 200, "{view}");
    let data = if view["data"].is_object() {
        &view["data"]
    } else {
        &view
    };
    let generation = data["generation"].as_str().expect("generation").to_owned();
    let (status, sent) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/console/command"),
        json!({"expected_generation":generation,"line":"say imported by Vesta Server"}),
        &format!("{}\r\nIdempotency-Key: import-console-say-0001", host.owner),
    );
    assert!((200..300).contains(&status), "{status} {sent}");
    let (status, players) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/players"),
        Value::Null,
        &host.owner,
    );
    assert!(status < 500, "{status} {players}");
    let (status, stopped) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/lifecycle/stop"),
        json!({"expected_generation":generation}),
        &format!(
            "{}\r\nIdempotency-Key: import-lifecycle-stop-0001",
            host.owner
        ),
    );
    assert!((200..300).contains(&status), "{status} {stopped}");
    let until = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let (_, list) = host.call("GET", "/v1/host/servers", Value::Null, &host.owner);
        if list["servers"][0]["state"] == "stopped" {
            break;
        }
        assert!(std::time::Instant::now() < until, "not stopped: {list}");
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        host.state
            .server(server.parse().unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn create_catalogue_and_defaults_are_machine_level_and_owner_only() {
    let host = Host::new(Arc::new(FakeEnv::default()));
    for path in [
        "/v1/host/catalogue/minecraft",
        "/v1/host/catalogue/software?minecraftVersion=26.1",
        "/v1/host/creates/defaults",
    ] {
        let (status, value) = host.call("GET", path, Value::Null, &host.stranger);
        assert_eq!(status, 403, "{value}");
    }
    let (status, value) = host.call(
        "GET",
        "/v1/host/catalogue/minecraft",
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["latestStable"], "26.1");
    assert_eq!(value["versions"][0]["experimental"], true);
    let (status, value) = host.call(
        "GET",
        "/v1/host/catalogue/software?minecraftVersion=26.1",
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{value}");
    let kinds = value["kinds"].as_array().unwrap();
    assert_eq!(kinds.len(), 10);
    assert_eq!(kinds[0]["kind"], "vanilla");
    assert_eq!(kinds[0]["available"], true);
    assert_eq!(kinds[0]["builds"][0]["recommended"], true);
    assert_eq!(kinds[9]["available"], false);
    assert_eq!(kinds[9]["unavailableReason"], "provider_unavailable");
    assert_eq!(kinds[8]["available"], false);
    assert_eq!(kinds[8]["unavailableReason"], "unsupported_version");
    assert!(kinds[0]["unavailableReason"].is_null());
    assert_eq!(kinds[2]["builds"][0]["recommended"], false);
    let (status, value) = host.call("GET", "/v1/host/creates/defaults", Value::Null, &host.owner);
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["name"], "Minecraft Server");
    assert_eq!(value["minecraftVersion"], "26.1");
    assert_eq!(value["gamePort"], 25565);
    assert_eq!(value["memoryMiB"], 6144);
    assert_eq!(value["java"], "automatic");
    assert!(host.router.workers.lock().unwrap().is_empty());
}

#[test]
fn create_catalogue_validates_query_and_does_not_recommend_experimental() {
    let host = Host::new(Arc::new(FakeEnv::default()));
    for query in [
        "",
        "minecraftVersion=",
        "minecraftVersion=missing",
        "minecraftVersion=26.1&minecraftVersion=26.1",
        "minecraftVersion=..%2Fsecret",
        "x=26.1",
    ] {
        let (status, value) = host.call(
            "GET",
            &format!("/v1/host/catalogue/software?{query}"),
            Value::Null,
            &host.owner,
        );
        assert_eq!(status, 400, "{query}: {value}");
    }
    let (status, value) = host.call(
        "GET",
        "/v1/host/catalogue/software?minecraftVersion=26.2-snapshot-1",
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{value}");
    for kind in value["kinds"].as_array().unwrap() {
        assert_eq!(kind["recommended"], false);
        for build in kind["builds"].as_array().unwrap() {
            assert_eq!(build["recommended"], false);
        }
    }
}

#[path = "create_tests.rs"]
mod creates;
