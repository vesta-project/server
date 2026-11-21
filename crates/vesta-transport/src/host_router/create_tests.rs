//! `new` server creation through the host router with a fake installer seam
//! (no downloads; the "game" is this test binary acting as an inert fixture),
//! and one ignored real end-to-end creation of Vanilla plus Fabric.
use super::*;
use std::sync::atomic::AtomicBool;
use vesta_control::machine::creates::{
    CreateInstaller, CreatePackIssue, CreatePackReview, CreateProgress, InstallRequest,
    PackSourceRequest,
};
use vesta_server_install::{
    JavaRequirement, ManagedLaunch, MojangObject, ResolvedPreview, Selection, Software, Warning,
};

const FIXTURE_MARKER: &str = "vesta-create-fixture";
const FIXTURE_ENABLE: &[u8] = b"vesta-owned-inert-create-fixture-v1";
const FIXTURE_TEST: &str = "host_router::tests::imports::creates::inert_game_fixture";
const FIXTURE_EXIT: &str = "fixture-exit";

/// Inert stand-in for a Minecraft server, run by the worker as the launch
/// executable inside a created game directory. Outside such a directory (a
/// normal test run) it returns at once.
#[test]
fn inert_game_fixture() {
    use std::io::{BufRead, Write};
    if std::fs::read(FIXTURE_MARKER).ok().as_deref() != Some(FIXTURE_ENABLE) {
        return;
    }
    let properties = std::fs::read_to_string("server.properties").unwrap();
    let port: u16 = properties
        .lines()
        .find_map(|l| l.strip_prefix("server-port="))
        .unwrap()
        .parse()
        .unwrap();
    let _status = vesta_game_runtime::offline::Responder::bind(
        vesta_game_runtime::offline::Configuration {
            address: ([127, 0, 0, 1], port).into(),
            status_json: r#"{"version":{"name":"Inert fixture","protocol":767},"players":{"online":0,"max":20},"description":{"text":"Inert fixture"}}"#.into(),
            disconnect: "Inert fixture".into(),
            wake: false,
        },
    )
    .unwrap();
    // A service stop ends the game with the service (systemd stops the whole
    // unit): tests model that by creating `fixture-exit` in the game directory.
    std::thread::spawn(|| {
        loop {
            if std::path::Path::new(FIXTURE_EXIT).exists() {
                std::process::exit(0);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    // libtest has printed "test <name> ... " without a newline: start a line.
    println!();
    println!("[12:00:00] [Server thread/INFO]: Done (1.234s)! For help, type \"help\"");
    std::io::stdout().flush().unwrap();
    let mut commands = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("fixture-commands")
        .unwrap();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        writeln!(commands, "{line}").unwrap();
        if let Some(text) = line.strip_prefix("say ") {
            println!("[12:00:01] [Server thread/INFO]: [Server] {text}");
            std::io::stdout().flush().unwrap();
        }
        if line == "stop" {
            std::process::exit(0);
        }
    }
}

/// Deterministic installer: fixed resolutions, "Java" is this test binary and
/// installation copies it as the server software.
#[derive(Default)]
struct FakeInstaller {
    /// While set, an install waits (checking cancellation) after it starts
    /// downloading, so tests can observe phases and cancel mid-install.
    hold: AtomicBool,
    /// Never return from an install (simulates a crash for restart tests).
    hang: AtomicBool,
    installs: AtomicU64,
    /// What Mojang's own manifest says about the reviewed objects (None: they match).
    mojang: std::sync::Mutex<Option<&'static str>>,
    /// Mojang verifications requested.
    mojang_checks: AtomicU64,
    /// The machine has no git (CraftBukkit/Spigot cannot be built).
    no_git: AtomicBool,
    /// `jdk` of every Java plan requested.
    jdk_plans: std::sync::Mutex<Vec<bool>>,
    /// Pack content installs performed (the reviewed pack of each).
    pack_installs: std::sync::Mutex<Vec<CreatePackReview>>,
    /// Review uploads and install packs with the live pack engine (in the
    /// new server's own worker), on top of this fake's base software.
    live_packs: AtomicBool,
    /// With `live_packs`: the pack install waits until the job is cancelled.
    pack_hold: AtomicBool,
    /// Set once a live pack install began (base software published).
    pack_started: AtomicBool,
}
/// Fixture packs by project id: a Fabric pack whose Minecraft version and
/// loader come from the pack; `client-only`, `notices`, `blocking` and
/// `offline` vary the review.
fn fixture_pack(project: &str) -> Result<CreatePackReview, &'static str> {
    let mut review = CreatePackReview {
        name: "Fixture Pack".into(),
        minecraft_version: "26.1".into(),
        loader: "fabric".into(),
        loader_version: Some("0.16.9".into()),
        client_only: false,
        issues: Vec::new(),
        content_bytes: Some(2048),
        source_digest: format!("provider:{project}"),
        mods_without_loader: false,
    };
    match project {
        "offline" => return Err("provider_unavailable"),
        "client-only" => review.client_only = true,
        "notices" => review.issues.push(CreatePackIssue {
            code: "pack_size_warning".into(),
            message: "pack_size_warning:2048".into(),
            blocking: false,
        }),
        "blocking" => review.issues.push(CreatePackIssue {
            code: "pack_path_invalid".into(),
            message: "executable_directory: start.sh".into(),
            blocking: true,
        }),
        _ => {}
    }
    Ok(review)
}
impl CreateInstaller for FakeInstaller {
    fn review_pack(
        &self,
        source: &PackSourceRequest,
        upload: Option<&Path>,
    ) -> Result<CreatePackReview, &'static str> {
        match (source, upload) {
            (PackSourceRequest::Provider { project_id, .. }, None) => fixture_pack(project_id),
            (PackSourceRequest::Upload { .. }, Some(path))
                if self.live_packs.load(Ordering::SeqCst) =>
            {
                vesta_control::packs::workspace::review_create_uploaded_pack(path)
            }
            (PackSourceRequest::Upload { .. }, Some(path)) => {
                // The real upload review parses the archive; the fixture only
                // binds its digest like the live review does.
                use sha2::Digest;
                let bytes = std::fs::read(path).unwrap();
                Ok(CreatePackReview {
                    name: "Uploaded Pack".into(),
                    source_digest: hex::encode(sha2::Sha256::digest(&bytes)),
                    content_bytes: None,
                    ..fixture_pack("uploaded").unwrap()
                })
            }
            _ => panic!("source and upload disagree"),
        }
    }
    fn install_pack(
        &self,
        request: &vesta_control::machine::creates::PackInstallRequest<'_>,
        progress: &dyn CreateProgress,
    ) -> Result<(), &'static str> {
        if self.live_packs.load(Ordering::SeqCst) {
            progress.phase("installing");
            self.pack_started.store(true, Ordering::SeqCst);
            let until = std::time::Instant::now() + Duration::from_secs(60);
            while self.pack_hold.load(Ordering::SeqCst) && !progress.cancelled() {
                assert!(std::time::Instant::now() < until, "never cancelled");
                std::thread::sleep(Duration::from_millis(20));
            }
            let servers = request.server_dir.parent().unwrap().to_path_buf();
            let java = servers.join(".unused-java-cache");
            let result = vesta_control::machine::creates::LiveCreateInstaller::new(java, servers)
                .install_pack(request, progress);
            if result.is_ok() {
                self.pack_installs
                    .lock()
                    .unwrap()
                    .push(request.review.clone());
            }
            return result;
        }
        // Unregistered, with the base software already published.
        assert!(request.server_dir.join("game/server.properties").is_file());
        assert!(request.server_dir.join("private/launch.toml").is_file());
        assert_eq!(request.policy.server_id, request.server_id);
        progress.phase("installing");
        let mods = request.server_dir.join("game/config");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("fixture-pack.txt"), b"pack content").unwrap();
        self.pack_installs
            .lock()
            .unwrap()
            .push(request.review.clone());
        Ok(())
    }
    fn verify_mojang(&self, preview: &ResolvedPreview) -> Result<(), &'static str> {
        assert!(preview.legacy_checksum && preview.legacy_checksum_mojang_only);
        assert!(!preview.mojang_objects.is_empty());
        self.mojang_checks.fetch_add(1, Ordering::SeqCst);
        match *self.mojang.lock().unwrap() {
            Some(code) => Err(code),
            None => Ok(()),
        }
    }
    fn resolve(
        &self,
        software: Software,
        selection: &Selection,
    ) -> Result<ResolvedPreview, &'static str> {
        let version = selection.version.clone().ok_or("software_unresolved")?;
        if software == Software::Spigot {
            return Err("provider_unavailable");
        }
        let loader = matches!(software, Software::Fabric | Software::Quilt)
            .then(|| selection.loader_version.clone().unwrap_or("0.16.10".into()));
        let build = matches!(software, Software::Paper | Software::Purpur)
            .then(|| selection.build.unwrap_or(42));
        Ok(ResolvedPreview {
            software,
            artifacts_sha256: format!("{software:?}-{version}-{build:?}-{loader:?}"),
            version,
            build,
            loader_version: loader,
            required_java: JavaRequirement {
                major: 21,
                evidence: "fake".into(),
            },
            warnings: vec![Warning::FirstRunEffects],
            // Vanilla: Mojang's SHA-1-only objects. Purpur: a third-party MD5.
            legacy_checksum: matches!(software, Software::Vanilla | Software::Purpur),
            legacy_checksum_mojang_only: software != Software::Purpur,
            download_bytes: Some(1024),
            mojang_objects: if software == Software::Vanilla {
                vec![MojangObject {
                    url: "https://piston-data.mojang.com/v1/objects/0123456789abcdef0123456789abcdef01234567/server.jar".into(),
                    sha1: "0123456789abcdef0123456789abcdef01234567".into(),
                }]
            } else {
                Vec::new()
            },
        })
    }
    fn source_build_tools(&self) -> bool {
        !self.no_git.load(Ordering::SeqCst)
    }
    fn plan_java(&self, required_major: u32, jdk: bool) -> vesta_control::java::HostJavaPlan {
        self.jdk_plans.lock().unwrap().push(jdk);
        vesta_control::java::plan_for_major_image(&[], Some(required_major), jdk)
    }
    fn ensure_java(&self, runtime: &ReviewRuntime) -> Result<PathBuf, &'static str> {
        assert_eq!(runtime.required_major, Some(21));
        Ok(std::env::current_exe().unwrap())
    }
    fn install(
        &self,
        request: &InstallRequest<'_>,
        progress: &dyn CreateProgress,
        publish: &mut dyn FnMut() -> Result<PathBuf, &'static str>,
    ) -> Result<ManagedLaunch, &'static str> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        progress.phase("downloading_software");
        progress.bytes(512, Some(1024));
        while self.hold.load(Ordering::SeqCst) || self.hang.load(Ordering::SeqCst) {
            if progress.cancelled() && !self.hang.load(Ordering::SeqCst) {
                return Err("cancelled");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        progress.bytes(1024, Some(1024));
        progress.phase("installing");
        // The "server software" is an owned copy of this test executable.
        let software = request.install_root.join("software");
        std::fs::create_dir_all(&software).unwrap();
        // Live pack installs need the server's software service, which only
        // manages launches whose executable is named `java`.
        let live = self.live_packs.load(Ordering::SeqCst);
        let exe = software.join(match (live, cfg!(windows)) {
            (true, true) => "java.exe",
            (true, false) => "java",
            (false, true) => "fixture.exe",
            (false, false) => "fixture",
        });
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let game = publish()?;
        let game = std::fs::canonicalize(game).unwrap();
        let game = plain(&game);
        std::fs::write(game.join(FIXTURE_MARKER), FIXTURE_ENABLE).unwrap();
        let exe = plain(&std::fs::canonicalize(exe).unwrap());
        Ok(ManagedLaunch {
            java: exe.clone(),
            working_directory: game,
            args: vec![
                "--exact".into(),
                FIXTURE_TEST.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            required_files: vec![exe],
            artifacts: Vec::new(),
        })
    }
    /// The fixture is not a JVM; it would reject `-Xmx`.
    fn heap_args(&self, _: u64) -> Vec<String> {
        Vec::new()
    }
}

thread_local! { static PORTS: std::cell::RefCell<std::collections::HashMap<u16,u16>> = std::cell::RefCell::new(std::collections::HashMap::new()); }
fn test_port(port: u16) -> u16 {
    PORTS.with(|ports| *ports.borrow_mut().entry(port).or_insert_with(free_port))
}

impl Host {
    fn create_wait(&self, job: &str, timeout: Duration, done: impl Fn(&Value) -> bool) -> Value {
        let until = std::time::Instant::now() + timeout;
        loop {
            let (status, value) = self.call_until(
                "GET",
                &format!("/v1/host/creates/jobs/{job}"),
                Value::Null,
                &self.owner,
                until,
            );
            assert_eq!(status, 200, "{value}");
            if done(&value) {
                return value;
            }
            assert!(
                std::time::Instant::now() < until,
                "job did not settle: {value}"
            );
            std::thread::sleep(
                Duration::from_secs(1)
                    .min(until.saturating_duration_since(std::time::Instant::now())),
            );
        }
    }

    fn create_plan(&self, mut body: Value) -> Value {
        if let Some(port) = body["gamePort"].as_u64().filter(|p| *p > 0 && *p <= 65535) {
            body["gamePort"] = json!(if (25570..=25999).contains(&port) {
                test_port(port as u16)
            } else {
                port as u16
            });
        } else if body["gamePort"].is_null() {
            body["gamePort"] = json!(free_port());
        }
        let (status, accepted) = self.call("POST", "/v1/host/creates/plan", body, &self.owner);
        assert_eq!(status, 202, "{accepted}");
        assert_eq!(accepted["state"], "planning");
        let id = accepted["jobId"].as_str().unwrap().to_owned();
        self.create_wait(&id, Duration::from_secs(120), |j| j["state"] != "planning")
    }
    fn create_apply(&self, job: &Value, key: &str, extra: Value) -> (u16, Value) {
        let review = &job["review"];
        let mut body = json!({"planId":review["planId"],"confirmation":review["confirmation"],"acknowledgedWarnings":review["warnings"],"eulaAccepted":true});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        self.call(
            "POST",
            "/v1/host/creates/apply",
            body,
            &format!("{}\r\nIdempotency-Key: {key}", self.owner),
        )
    }
    fn job_action(&self, id: &str, action: &str) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/v1/host/creates/jobs/{id}/{action}"),
            Value::Null,
            &self.owner,
        )
    }
    /// Raw server pack upload with `X-Upload-Kind`.
    fn upload(&self, auth: &str, kind: &str, body: &[u8]) -> (u16, Value) {
        let (status, bytes, _) = self.wire().raw(
            "POST",
            "/v1/host/creates/uploads",
            &format!(
                "Content-Type: application/octet-stream\r\nX-Upload-Kind: {kind}\r\nAuthorization: {auth}\r\n"
            ),
            body,
        );
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    fn servers(&self) -> Vec<Value> {
        let (status, list) = self.call("GET", "/v1/host/servers", Value::Null, &self.owner);
        assert_eq!(status, 200, "{list}");
        list["servers"].as_array().unwrap().clone()
    }
}

fn new_body(name: &str, port: Option<u16>, memory: Option<u64>) -> Value {
    json!({"kind":"new","name":name,"minecraftVersion":"26.1",
        "software":{"kind":"vanilla","build":null,"loaderVersion":null},
        "gamePort":port,"memoryMiB":memory,"eulaAccepted":false})
}
fn create_host(installer: Arc<FakeInstaller>) -> Host {
    Host::start(
        tempfile::tempdir().unwrap(),
        fake_env(),
        Some(installer),
        None,
        None,
    )
}

#[test]
fn owner_creates_vanilla_reviews_applies_starts_and_reaches_online() {
    let installer = Arc::new(FakeInstaller::default());
    installer.hold.store(true, Ordering::SeqCst);
    let host = create_host(installer.clone());
    let (_, info) = host.call("GET", "/v1/host", Value::Null, &host.owner);
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("create-jobs"))
    );
    let port = free_port();
    let job = host.create_plan(new_body("  Survival ", Some(port), Some(1024)));
    assert_eq!(job["state"], "review", "{job}");
    assert_eq!(job["phase"], "resolving");
    assert!(job["serverId"].is_null());
    let id = job["jobId"].as_str().unwrap().to_owned();
    let review = &job["review"];
    assert!(review["confirmation"].is_string());
    assert!(review["expiresAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(review["blockers"], json!([]));
    // Mojang's SHA-1-only objects are accepted by the host, not the owner.
    assert_eq!(review["warnings"], json!([]));
    let summary = &review["summary"];
    assert_eq!(summary["kind"], "new");
    assert_eq!(summary["name"], "Survival");
    assert_eq!(summary["minecraftVersion"], "26.1");
    assert_eq!(
        summary["software"],
        json!({"kind":"vanilla","version":"26.1","build":null,"loaderVersion":null})
    );
    assert_eq!(
        summary["resources"],
        json!({"gamePort":port,"memoryMiB":1024})
    );
    assert_eq!(
        summary["runtime"],
        json!({"requiredMajor":21,"downloadRequired":true,"automatic":true})
    );
    assert!(summary["pack"].is_null() && summary["restore"].is_null());
    let server = summary["destination"]["serverId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        summary["destination"]["displayPath"]
            .as_str()
            .unwrap()
            .contains(&server)
    );

    // Apply needs a key, the EULA and every warning acknowledged.
    let (status, error) = host.call(
        "POST",
        "/v1/host/creates/apply",
        json!({"planId":review["planId"],"confirmation":review["confirmation"],"acknowledgedWarnings":review["warnings"],"eulaAccepted":true}),
        &host.owner,
    );
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("idempotency_key_required"))
    );
    let (status, error) = host.create_apply(&job, "eula-key", json!({"eulaAccepted":false}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("eula_required"))
    );
    let (status, error) = host.create_apply(
        &job,
        "warn-key",
        json!({"acknowledgedWarnings":["legacy_checksum"]}),
    );
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("warnings_unacknowledged"))
    );
    let (status, error) =
        host.create_apply(&job, "confirm-key", json!({"confirmation":"not-the-token"}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (409, json!("confirmation_invalid"))
    );
    let (status, accepted) = host.create_apply(&job, "create-key-1", json!({}));
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(accepted, json!({"jobId":id,"serverId":server}));
    // Exact replay returns the same reply; another body with the key conflicts.
    assert_eq!(
        host.create_apply(&job, "create-key-1", json!({})),
        (202, accepted.clone())
    );
    let (status, error) = host.create_apply(
        &job,
        "create-key-1",
        json!({"acknowledgedWarnings":["other"]}),
    );
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (409, json!("idempotency_conflict"))
    );
    // A second key cannot apply the same plan again.
    assert_eq!(host.create_apply(&job, "create-key-2", json!({})).0, 409);

    // Phases and byte progress while downloading.
    let downloading = host.create_wait(&id, Duration::from_secs(30), |j| {
        j["phase"] == "downloading_software" && j["progress"]["bytesDone"] == 512
    });
    assert_eq!(downloading["state"], "applying");
    assert_eq!(downloading["progress"]["bytesTotal"], 1024);
    assert!(downloading["review"]["confirmation"].is_null());
    installer.hold.store(false, Ordering::SeqCst);
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    assert_eq!(ready["phase"], "starting");
    assert!(ready["error"].is_null());
    assert_eq!(ready["serverId"], server);
    assert_eq!(installer.installs.load(Ordering::SeqCst), 1);

    // Files: owner EULA record, reserved port, frozen launch, host document.
    let dir = host.temp.path().join("servers").join(&server);
    let eula = std::fs::read_to_string(dir.join("game/eula.txt")).unwrap();
    assert!(eula.contains("eula=true") && eula.contains("Accepted by Vesta Server owner"));
    let properties = std::fs::read_to_string(dir.join("game/server.properties")).unwrap();
    assert!(properties.contains(&format!("server-port={port}")));
    assert!(properties.contains("enable-rcon=false"));
    assert!(!dir.join("staging").exists());
    let launch = std::fs::read_to_string(dir.join("private/launch.toml")).unwrap();
    assert!(launch.contains(FIXTURE_TEST), "{launch}");
    assert!(
        std::fs::read_to_string(dir.join("host.toml"))
            .unwrap()
            .contains("Survival")
    );

    // Listed once, with software and Minecraft version, and no icon yet.
    let servers = host.servers();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["serverId"], server);
    assert_eq!(servers[0]["name"], "Survival");
    assert_eq!(servers[0]["state"], "online");
    assert_eq!(
        servers[0]["software"],
        json!({"kind":"vanilla","version":"26.1"})
    );
    assert_eq!(servers[0]["minecraftVersion"], "26.1");
    assert!(servers[0]["iconEtag"].is_null());
    // Committed: cancel is refused, resume is only for interrupted jobs.
    assert_eq!(host.job_action(&id, "cancel").0, 409);
    assert_eq!(host.job_action(&id, "resume").0, 409);

    // Start returns at once; readiness is polled until the game is online.
    let (status, started) = host.job_action(&id, "start");
    assert_eq!(status, 202, "{started}");
    assert_eq!(started["state"], "online", "{started}");
    assert_eq!(started["phase"], "starting");
    let online = host.create_wait(&id, Duration::from_secs(120), |j| j["state"] != "starting");
    assert_eq!(online["state"], "online", "{online}");
    assert_eq!(host.servers()[0]["state"], "online");
    let software = software_status(&host, &server);
    assert_eq!(software["current"]["software"], "vanilla", "{software}");
    assert_eq!(software["current"]["version"], "26.1", "{software}");

    // The server's management routes work at once, including its icon.
    let icon = format!("/v1/servers/{server}/manage/server/icon");
    let (status, _, _) = host.wire().raw(
        "GET",
        &icon,
        &format!("Authorization: {}\r\n", host.owner),
        b"",
    );
    assert_eq!(status, 404);
    let png = vesta_control::motd::fallback_icon();
    let (status, body, _) = host.wire().raw(
        "PUT",
        &icon,
        &format!(
            "Authorization: {}\r\nIdempotency-Key: create-icon-0001\r\nIf-None-Match: *\r\nContent-Type: image/png\r\n",
            host.owner
        ),
        &png,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let (status, served, etag) = host.wire().raw(
        "GET",
        &icon,
        &format!("Authorization: {}\r\n", host.owner),
        b"",
    );
    assert_eq!(status, 200);
    assert_eq!(served, png);
    let etag = etag.unwrap();
    assert_eq!(
        etag,
        vesta_control::identity::management::permissions::branding::icon_etag(&png)
    );
    assert_eq!(host.servers()[0]["iconEtag"], etag);

    // Console command, then a stop through the mapped lifecycle route.
    let (status, view) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/server/status"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{view}");
    let data = if view["data"].is_object() {
        &view["data"]
    } else {
        &view
    };
    assert_eq!(data["on_agent_start"], "restore_previous");
    let generation = data["generation"].as_str().expect("generation").to_owned();
    let (status, sent) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/console/command"),
        json!({"expected_generation":generation,"line":"say created by Vesta Server"}),
        &format!("{}\r\nIdempotency-Key: create-console-say-0001", host.owner),
    );
    assert!((200..300).contains(&status), "{status} {sent}");
    let (status, stopped) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/lifecycle/stop"),
        json!({"expected_generation":generation}),
        &format!(
            "{}\r\nIdempotency-Key: create-lifecycle-stop-0001",
            host.owner
        ),
    );
    assert!((200..300).contains(&status), "{status} {stopped}");
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while host.servers()[0]["state"] != "stopped" {
        assert!(std::time::Instant::now() < until, "not stopped");
        std::thread::sleep(Duration::from_millis(200));
    }
    let commands = std::fs::read_to_string(dir.join("game/fixture-commands")).unwrap();
    assert!(
        commands.contains("say created by Vesta Server"),
        "{commands}"
    );
}

#[test]
fn create_routes_are_owner_only_and_unbuilt_modes_answer_not_supported() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let job = host.create_plan(new_body("Owned", None, None));
    let id = job["jobId"].as_str().unwrap();
    for (method, path, body) in [
        (
            "POST",
            "/v1/host/creates/plan".to_owned(),
            new_body("x", None, None),
        ),
        ("POST", "/v1/host/creates/apply".to_owned(), json!({})),
        ("GET", format!("/v1/host/creates/jobs/{id}"), Value::Null),
        (
            "POST",
            format!("/v1/host/creates/jobs/{id}/cancel"),
            Value::Null,
        ),
        (
            "POST",
            format!("/v1/host/creates/jobs/{id}/resume"),
            Value::Null,
        ),
        (
            "POST",
            format!("/v1/host/creates/jobs/{id}/start"),
            Value::Null,
        ),
        (
            "GET",
            "/v1/host/creates/restore-sources".to_owned(),
            Value::Null,
        ),
        ("POST", "/v1/host/creates/uploads".to_owned(), Value::Null),
    ] {
        let (status, value) = host.call(method, &path, body, &host.stranger);
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (403, json!("forbidden")),
            "{method} {path}"
        );
    }
    // Restore is built: a malformed source id is refused, the listing works.
    let (status, value) = host.call(
        "POST",
        "/v1/host/creates/plan",
        json!({"kind":"restore","name":"Back","source":{"kind":"snapshot","sourceId":"s"},"gamePort":25566,"memoryMiB":4096,"eulaAccepted":false}),
        &host.owner,
    );
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (400, json!("invalid_request"))
    );
    let (status, value) = host.call(
        "GET",
        "/v1/host/creates/restore-sources",
        Value::Null,
        &host.owner,
    );
    assert_eq!((status, value), (200, json!({"sources":[]})));
    // Uploaded recovery exports remain 501.
    let (status, value) = host.upload(&host.owner, "restore", b"PK");
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (501, json!("not_supported_yet"))
    );
    // Invalid shapes are 400; unknown jobs 404 with the create code.
    for body in [
        json!({"kind":"other"}),
        json!({"kind":"new"}),
        json!({"kind":"new","name":"x","minecraftVersion":"26.1","software":{"kind":"vanilla","build":7,"loaderVersion":null},"gamePort":null,"memoryMiB":null}),
        json!({"kind":"new","name":"x","minecraftVersion":"26.1","software":{"kind":"bedrock","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}),
        json!({"kind":"new","name":" ","minecraftVersion":"26.1","software":{"kind":"vanilla","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}),
        json!({"kind":"new","name":"x","minecraftVersion":"../x","software":{"kind":"vanilla","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}),
        json!({"kind":"new","name":"x","minecraftVersion":"26.1","software":{"kind":"vanilla","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null,"javaPath":"/bin/sh"}),
    ] {
        let (status, value) = host.call("POST", "/v1/host/creates/plan", body.clone(), &host.owner);
        assert_eq!(status, 400, "{body}: {value}");
    }
    let (status, value) = host.call(
        "GET",
        &format!("/v1/host/creates/jobs/{}", Uuid::new_v4()),
        Value::Null,
        &host.owner,
    );
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (404, json!("create_job_not_found"))
    );
    // Jobs of one flow are unknown to the other's routes.
    assert_eq!(
        host.call(
            "GET",
            &format!("/v1/host/imports/jobs/{id}"),
            Value::Null,
            &host.owner
        )
        .0,
        404
    );
    // A Minecraft version the catalogue does not offer, or a provider outage,
    // blocks the review without a usable confirmation.
    let unknown = host.create_plan(json!({"kind":"new","name":"Old","minecraftVersion":"1.0.0",
        "software":{"kind":"vanilla","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}));
    assert_eq!(unknown["state"], "blocked");
    assert_eq!(
        unknown["review"]["blockers"],
        json!(["software_unresolved"])
    );
    assert!(unknown["review"]["confirmation"].is_null());
    let outage = host.create_plan(json!({"kind":"new","name":"Plugins","minecraftVersion":"26.1",
        "software":{"kind":"spigot","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}));
    assert_eq!(
        outage["review"]["blockers"],
        json!(["provider_unavailable"])
    );
    let (status, error) = host.create_apply(&outage, "blocked-key", json!({"confirmation":"none"}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (409, json!("plan_blocked"))
    );
    // Legacy digests from a source other than Mojang still need acknowledging.
    let purpur = host.create_plan(json!({"kind":"new","name":"Purple","minecraftVersion":"26.1",
        "software":{"kind":"purpur","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}));
    assert_eq!(purpur["review"]["warnings"], json!(["legacy_checksum"]));
    // Snapshots are experimental and need acknowledgement.
    let snapshot = host.create_plan(json!({"kind":"new","name":"Preview","minecraftVersion":"26.2-snapshot-1",
        "software":{"kind":"paper","build":null,"loaderVersion":null},"gamePort":null,"memoryMiB":null}));
    assert_eq!(snapshot["state"], "review", "{snapshot}");
    assert_eq!(
        snapshot["review"]["warnings"],
        json!(["experimental_software"])
    );
    assert_eq!(snapshot["review"]["summary"]["software"]["version"], "42");
    // Cancelling a review keeps nothing.
    let (status, cancelled) = host.job_action(id, "cancel");
    assert_eq!((status, cancelled), (200, json!({"state":"cancelled"})));
    assert_eq!(host.create_apply(&job, "cancelled-key", json!({})).0, 409);
    assert!(host.servers().is_empty());
}

#[test]
fn cancel_mid_install_releases_the_reservation_and_destination() {
    let installer = Arc::new(FakeInstaller::default());
    installer.hold.store(true, Ordering::SeqCst);
    let host = create_host(installer.clone());
    let job = host.create_plan(new_body("Survival", Some(25570), Some(2048)));
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "cancel-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    host.create_wait(&id, Duration::from_secs(30), |j| {
        j["phase"] == "downloading_software"
    });
    assert!(host.temp.path().join("servers").join(&server).exists());
    // While reserved, the same port and name are refused by a new plan.
    let rival = host.create_plan(new_body("survival", Some(25570), Some(1024)));
    assert_eq!(
        rival["review"]["blockers"],
        json!(["game_port_in_use", "name_in_use"])
    );
    let (status, requested) = host.job_action(&id, "cancel");
    assert_eq!((status, requested), (200, json!({"state":"applying"})));
    let done = host.create_wait(&id, Duration::from_secs(30), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(done["state"], "cancelled", "{done}");
    assert!(!host.temp.path().join("servers").join(&server).exists());
    assert!(host.servers().is_empty());
    // Released: the same name, port and memory plan cleanly again.
    installer.hold.store(false, Ordering::SeqCst);
    let again = host.create_plan(new_body("Survival", Some(25570), Some(2048)));
    assert_eq!(again["state"], "review", "{again}");
    assert_eq!(
        host.job_action(&id, "cancel").1,
        json!({"state":"cancelled"})
    );
}

#[test]
fn back_to_back_creates_get_distinct_resources_and_survive_a_restart() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let mut created = Vec::new();
    for key in ["first-key", "second-key"] {
        let (status, defaults) =
            host.call("GET", "/v1/host/creates/defaults", Value::Null, &host.owner);
        assert_eq!(status, 200, "{defaults}");
        let job = host.create_plan(new_body(
            defaults["name"].as_str().unwrap(),
            defaults["gamePort"].as_u64().map(|p| p as u16),
            defaults["memoryMiB"].as_u64(),
        ));
        assert_eq!(job["state"], "review", "{job}");
        let (status, accepted) = host.create_apply(&job, key, json!({}));
        assert_eq!(status, 202, "{accepted}");
        let id = accepted["jobId"].as_str().unwrap().to_owned();
        let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        });
        assert_eq!(ready["state"], "online", "{ready}");
        created.push((id, defaults));
    }
    let (first, second) = (&created[0].1, &created[1].1);
    assert_eq!(first["name"], "Minecraft Server");
    assert_eq!(second["name"], "Minecraft Server 2");
    assert_ne!(first["gamePort"], second["gamePort"]);
    // 16 GiB host: 12 GiB budget, both suggestions fit inside it together.
    let total = first["memoryMiB"].as_u64().unwrap() + second["memoryMiB"].as_u64().unwrap();
    assert!(total <= 12 * 1024, "{total}");
    let servers = host.servers();
    assert_eq!(servers.len(), 2);
    assert_ne!(servers[0]["gamePort"], servers[1]["gamePort"]);
    // The full per-server budget remains usable, with an advisory warning.
    let third = host.create_plan(new_body("Third", None, Some(12 * 1024)));
    assert_eq!(third["review"]["blockers"], json!([]));
    assert_eq!(third["review"]["warnings"], json!(["memory_overcommitted"]));
    assert_eq!(host.call("GET", "/v1/host/creates/defaults", Value::Null, &host.owner).0, 200);

    // Two plans racing for one free port: only the first apply wins.
    let host = host.restart(
        Arc::new(FakeEnv::default()),
        Some(Arc::new(FakeInstaller::default())),
    );
    // After the restart nothing is routed until the host restores workers.
    assert!(host.router.workers.lock().unwrap().is_empty());
    assert!(host.router.restore_registered_workers().is_empty());
    let routed: Vec<Uuid> = host
        .router
        .workers
        .lock()
        .unwrap()
        .iter()
        .map(|w| w.server_id())
        .collect();
    for server in host.servers() {
        let id: Uuid = server["serverId"].as_str().unwrap().parse().unwrap();
        assert!(routed.contains(&id), "{server}");
        let (status, view) = host.call(
            "GET",
            &format!("/v1/servers/{id}/manage/server/status"),
            Value::Null,
            &host.owner,
        );
        assert_eq!(status, 200, "{view}");
    }
    assert_eq!(routed.len(), 2);
    // Restoring twice adopts nothing new.
    assert!(host.router.restore_registered_workers().is_empty());
    assert_eq!(host.router.workers.lock().unwrap().len(), 2);
}

/// Card state of one server.
fn card_state(host: &Host, server: &str) -> Value {
    host.servers()
        .into_iter()
        .find(|s| s["serverId"] == server)
        .map(|s| s["state"].clone())
        .unwrap_or(Value::Null)
}

/// With the default startup policy a dedicated machine brings servers back
/// after the Vesta Server restarts (a reboot or an in-place update): one left
/// running is started again; one the owner stopped and one never started stay
/// stopped, and once the owner stops it the next restart leaves it stopped.
#[test]
fn running_servers_come_back_after_a_restart_and_stopped_ones_stay_stopped() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let mut servers = Vec::new();
    for (name, port) in [("Running", 25720), ("Stopped", 25721)] {
        let job = host.create_plan(new_body(name, Some(port), Some(1024)));
        let (status, accepted) = host.create_apply(&job, &format!("{name}-key"), json!({}));
        assert_eq!(status, 202, "{accepted}");
        let id = accepted["jobId"].as_str().unwrap().to_owned();
        let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        });
        assert_eq!(ready["state"], "online", "{ready}");
        if name != "Never" {
            assert_eq!(host.job_action(&id, "start").0, 202);
            let online =
                host.create_wait(&id, Duration::from_secs(60), |j| j["state"] != "starting");
            assert_eq!(online["state"], "online", "{online}");
        }
        servers.push(accepted["serverId"].as_str().unwrap().to_owned());
    }
    stop_server(&host, &servers[1], "stopped-stop-0001");
    assert_eq!(card_state(&host, &servers[0]), "online");

    // The Vesta Server stops: the OS ends its game with it (no lifecycle
    // operation is involved), then the service comes back.
    let game = host
        .temp
        .path()
        .join("servers")
        .join(&servers[0])
        .join("game");
    std::fs::write(game.join(FIXTURE_EXIT), b"").unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while card_state(&host, &servers[0]) == "online" {
        assert!(std::time::Instant::now() < until, "game did not end");
        std::thread::sleep(Duration::from_millis(200));
    }
    std::fs::remove_file(game.join(FIXTURE_EXIT)).unwrap();
    let host = host.restart(
        Arc::new(FakeEnv::default()),
        Some(Arc::new(FakeInstaller::default())),
    );
    assert!(host.router.restore_registered_workers().is_empty());
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while card_state(&host, &servers[0]) != "online" {
        assert!(
            std::time::Instant::now() < until,
            "not started again: {:?}",
            host.servers()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(host.router.restore_registered_workers().is_empty());
    // Give a wrongly started server time to show up.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(card_state(&host, &servers[1]), "stopped");
    // Never started: still only registered.
    stop_server(&host, &servers[0], "running-stop-0001");

    // Stopped by the owner before this restart: stays stopped too.
    let host = host.restart(
        Arc::new(FakeEnv::default()),
        Some(Arc::new(FakeInstaller::default())),
    );
    assert!(host.router.restore_registered_workers().is_empty());
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(card_state(&host, &servers[0]), "stopped");
    assert_eq!(card_state(&host, &servers[1]), "stopped");
}

#[test]
fn racing_plans_for_one_port_cannot_both_apply() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let a = host.create_plan(new_body("Alpha", Some(25580), Some(1024)));
    let b = host.create_plan(new_body("Beta", Some(25580), Some(1024)));
    assert_eq!(
        (a["state"].clone(), b["state"].clone()),
        (json!("review"), json!("review"))
    );
    assert_eq!(host.create_apply(&a, "race-a", json!({})).0, 202);
    let (status, error) = host.create_apply(&b, "race-b", json!({}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (409, json!("game_port_in_use"))
    );
    let c = host.create_plan(new_body("alpha", Some(25581), Some(1024)));
    let d = host.create_plan(new_body("Gamma", Some(25582), Some(1024)));
    assert_eq!(c["review"]["blockers"], json!(["name_in_use"]));
    assert_eq!(d["state"], "review");
}

#[test]
fn interrupted_create_resumes_after_a_restart_without_duplicates() {
    let hanging = Arc::new(FakeInstaller::default());
    hanging.hang.store(true, Ordering::SeqCst);
    let host = create_host(hanging);
    let job = host.create_plan(new_body("Phoenix", Some(25590), Some(1024)));
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "phoenix-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    host.create_wait(&id, Duration::from_secs(30), |j| {
        j["phase"] == "downloading_software"
    });
    // The process "dies" mid-download (its install never returns).
    let installer = Arc::new(FakeInstaller::default());
    let host = host.restart(Arc::new(FakeEnv::default()), Some(installer.clone()));
    let (status, job) = host.call(
        "GET",
        &format!("/v1/host/creates/jobs/{id}"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{job}");
    assert_eq!(job["state"], "interrupted", "{job}");
    assert!(host.servers().is_empty());
    // Start is refused until the install finished.
    assert_eq!(host.job_action(&id, "start").0, 409);
    let (status, resumed) = host.job_action(&id, "resume");
    assert_eq!(status, 202, "{resumed}");
    assert_eq!(resumed["state"], "applying");
    assert_eq!(resumed["phase"], "resolving");
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    assert_eq!(installer.installs.load(Ordering::SeqCst), 1);
    let servers = host.servers();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["serverId"], accepted["serverId"]);
    assert_eq!(servers[0]["gamePort"], test_port(25590));
    // Resume is for interrupted jobs only.
    assert_eq!(host.job_action(&id, "resume").0, 409);
}

/// Real creation: latest stable Vanilla through the router with the live
/// installer (verified managed Java and server download), online, console,
/// stop; then a Fabric server on another port, both online together.
#[test]
#[ignore = "downloads Java and server software, then runs two servers"]
fn real_vanilla_and_fabric_creates_run_together() {
    let scratch = tempfile::tempdir().unwrap();
    let java_cache = scratch.path().join("java");
    std::fs::create_dir_all(&java_cache).unwrap();
    let env = Arc::new(WindowsSafeEnv(HostEnvironment {
        java_cache: java_cache.clone(),
    }));
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("servers")).unwrap();
    let servers = std::fs::canonicalize(temp.path().join("servers")).unwrap();
    let installer = Arc::new(vesta_control::machine::creates::LiveCreateInstaller::new(
        java_cache, servers,
    ));
    let live =
        Arc::new(vesta_control::machine::creates::LiveCatalogue::open(scratch.path()).unwrap());
    let host = Host::start(temp, env, Some(installer), None, Some(live));
    let (status, catalogue) = host.call(
        "GET",
        "/v1/host/catalogue/minecraft",
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{catalogue}");
    let latest = catalogue["latestStable"].as_str().unwrap().to_owned();

    let mut running = Vec::new();
    for (name, kind) in [("Vanilla Real", "vanilla"), ("Fabric Real", "fabric")] {
        let port = free_port();
        let job = host.create_plan(json!({"kind":"new","name":name,"minecraftVersion":latest,
            "software":{"kind":kind,"build":null,"loaderVersion":null},
            "gamePort":port,"memoryMiB":1024,"eulaAccepted":false}));
        assert_eq!(job["state"], "review", "{job}");
        let id = job["jobId"].as_str().unwrap().to_owned();
        let (status, accepted) = host.create_apply(&job, &format!("real-{kind}"), json!({}));
        assert_eq!(status, 202, "{accepted}");
        let server = accepted["serverId"].as_str().unwrap().to_owned();
        let ready = host.create_wait(&id, Duration::from_secs(900), |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        });
        assert_eq!(ready["state"], "online", "{ready}");
        let (status, started) = host.job_action(&id, "start");
        assert_eq!(status, 202, "{started}");
        let online = host.create_wait(&id, Duration::from_secs(600), |j| j["state"] != "starting");
        if online["state"] != "online" {
            let dir = host.temp.path().join("servers").join(&server);
            eprintln!(
                "launch.toml:\n{}\nlatest.log:\n{}",
                std::fs::read_to_string(dir.join("private/launch.toml")).unwrap_or_default(),
                std::fs::read_to_string(dir.join("game/logs/latest.log")).unwrap_or_default()
            );
        }
        assert_eq!(online["state"], "online", "{online}");
        let software = software_status(&host, &server);
        assert_eq!(software["current"]["software"], kind, "{software}");
        assert_eq!(software["current"]["version"], latest, "{software}");
        let target = if kind == "vanilla" { "fabric" } else { "vanilla" };
        let (status, plan) = host.call("POST", &format!("/v1/servers/{server}/manage/software/plan"),
            json!({"software":target,"version":latest,"build":null,"loader_version":null}), &host.owner);
        if status != 200 { stop_server(&host, &server, &format!("failed-plan-stop-{i}", i=running.len())); }
        assert_eq!(status, 200, "{plan}");
        let plan=if plan["data"].is_object() {&plan["data"]} else {&plan};
        assert_eq!(plan["software"],target,"{plan}");
        assert!(plan["plan_id"].is_string(),"{plan}");
        running.push(server);
    }
    // Both online together, on distinct ports.
    let servers = host.servers();
    assert_eq!(servers.len(), 2);
    assert!(
        servers.iter().all(|s| s["state"] == "online"),
        "{servers:?}"
    );
    assert_ne!(servers[0]["gamePort"], servers[1]["gamePort"]);
    assert!(servers.iter().any(|s| s["software"]["kind"] == "fabric"));
    for (i, server) in running.iter().enumerate() {
        let (status, view) = host.call(
            "GET",
            &format!("/v1/servers/{server}/manage/server/status"),
            Value::Null,
            &host.owner,
        );
        assert_eq!(status, 200, "{view}");
        let data = if view["data"].is_object() {
            &view["data"]
        } else {
            &view
        };
        let generation = data["generation"].as_str().unwrap().to_owned();
        let (status, sent) = host.call(
            "POST",
            &format!("/v1/servers/{server}/manage/console/command"),
            json!({"expected_generation":generation,"line":"say created by Vesta Server"}),
            &format!("{}\r\nIdempotency-Key: real-create-say-000{i}", host.owner),
        );
        assert!((200..300).contains(&status), "{status} {sent}");
        let (status, stopped) = host.call(
            "POST",
            &format!("/v1/servers/{server}/manage/lifecycle/stop"),
            json!({"expected_generation":generation}),
            &format!("{}\r\nIdempotency-Key: real-create-stop-000{i}", host.owner),
        );
        assert!((200..300).contains(&status), "{status} {stopped}");
    }
    let until = std::time::Instant::now() + Duration::from_secs(180);
    while !host.servers().iter().all(|s| s["state"] == "stopped") {
        assert!(std::time::Instant::now() < until, "not stopped");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Java-style plain drive path: drop a Windows verbatim `\?\C:\` prefix.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

#[test]
fn vanilla_plan_has_no_warnings_and_the_host_accepts_mojang_sha1() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let job = host.create_plan(new_body("Plain", Some(25600), Some(1024)));
    assert_eq!(job["state"], "review", "{job}");
    assert_eq!(job["review"]["warnings"], json!([]));
    assert_eq!(job["review"]["blockers"], json!([]));
    let (status, accepted) = host.create_apply(&job, "plain-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    // The automatic acceptance is recorded in the host audit log.
    let id = accepted["jobId"].as_str().unwrap();
    let recorded: i64 = host
        .state
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM machine_audit WHERE action='create.mojang_sha1_accepted' AND target=?1",
                [format!("import:{id}")],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(recorded, 1);
}

#[test]
fn mojang_sha1_is_accepted_only_when_mojang_publishes_exactly_those_objects() {
    // Match: verified once, accepted by the host, no owner warning.
    let installer = Arc::new(FakeInstaller::default());
    let host = create_host(installer.clone());
    let job = host.create_plan(new_body("Verified", Some(25601), Some(1024)));
    assert_eq!(job["state"], "review", "{job}");
    assert_eq!(job["review"]["warnings"], json!([]));
    assert_eq!(installer.mojang_checks.load(Ordering::SeqCst), 1);

    // Mismatch: the mirror differs from Mojang's own data.
    *installer.mojang.lock().unwrap() = Some("integrity_conflict");
    let job = host.create_plan(new_body("Mismatch", Some(25602), Some(1024)));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["integrity_conflict"]));
    assert!(job["review"]["confirmation"].is_null());

    // Unreachable: never silent acceptance, the owner tries again.
    *installer.mojang.lock().unwrap() = Some("provider_unavailable");
    let job = host.create_plan(new_body("Offline", Some(25603), Some(1024)));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["provider_unavailable"]));
    let (status, error) =
        host.create_apply(&job, "offline-key", json!({"confirmation":"anything"}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (409, json!("plan_blocked"))
    );
    assert_eq!(installer.mojang_checks.load(Ordering::SeqCst), 3);

    // Purpur's third-party MD5 is never verified against Mojang: the owner
    // acknowledges it instead.
    *installer.mojang.lock().unwrap() = None;
    let mut body = new_body("Purpur", Some(25604), Some(1024));
    body["software"] = json!({"kind":"purpur","build":null,"loaderVersion":null});
    let job = host.create_plan(body);
    assert!(
        job["review"]["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("legacy_checksum")),
        "{job}"
    );
    assert_eq!(installer.mojang_checks.load(Ordering::SeqCst), 3);
}

#[test]
fn bukkit_without_git_blocks_at_planning_and_with_git_plans_a_managed_jdk() {
    let installer = Arc::new(FakeInstaller::default());
    installer.no_git.store(true, Ordering::SeqCst);
    let host = create_host(installer.clone());
    let mut body = new_body("Bukkit", Some(25610), Some(1024));
    body["software"] = json!({"kind":"bukkit","build":null,"loaderVersion":null});
    let job = host.create_plan(body.clone());
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["requires_git"]));
    assert!(job["review"]["confirmation"].is_null());
    // Nothing was resolved or installed: it never fails late.
    assert!(job["review"]["summary"]["software"]["version"].is_null());
    assert_eq!(installer.installs.load(Ordering::SeqCst), 0);

    // With git, a source build plans a JDK (Vesta's managed one here).
    installer.no_git.store(false, Ordering::SeqCst);
    let job = host.create_plan(body);
    assert_eq!(job["state"], "review", "{job}");
    assert_eq!(*installer.jdk_plans.lock().unwrap(), vec![true]);
    let runtime = &job["review"]["summary"]["runtime"];
    assert_eq!(runtime["downloadRequired"], true);
    // Plain servers keep the JRE.
    let job = host.create_plan(new_body("Plain", Some(25611), Some(1024)));
    assert_eq!(job["state"], "review", "{job}");
    assert_eq!(*installer.jdk_plans.lock().unwrap(), vec![true, false]);
}

fn modpack_body(name: &str, project: &str, port: u16) -> Value {
    json!({"kind":"modpack","name":name,
        "source":{"kind":"provider","provider":"modrinth","projectId":project,"versionId":"v1"},
        "gamePort":port,"memoryMiB":1024,"eulaAccepted":false})
}
/// A small zip with the given entry names.
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(body).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn provider_modpack_reviews_installs_its_content_and_reaches_online() {
    let installer = Arc::new(FakeInstaller::default());
    let host = create_host(installer.clone());
    let port = free_port();
    // Minecraft version and loader come from the pack, never the client.
    let mut guessed = modpack_body("Adventure", "pack", port);
    guessed["minecraftVersion"] = json!("1.20.1");
    let (status, error) = host.call("POST", "/v1/host/creates/plan", guessed, &host.owner);
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("invalid_request"))
    );

    let job = host.create_plan(modpack_body(" Adventure ", "pack", port));
    assert_eq!(job["state"], "review", "{job}");
    let review = &job["review"];
    assert_eq!(review["blockers"], json!([]));
    assert_eq!(review["warnings"], json!([]));
    let summary = &review["summary"];
    assert_eq!(summary["kind"], "modpack");
    assert_eq!(summary["name"], "Adventure");
    assert_eq!(summary["minecraftVersion"], "26.1");
    assert_eq!(
        summary["software"],
        json!({"kind":"fabric","version":"0.16.9","build":null,"loaderVersion":"0.16.9"})
    );
    assert_eq!(
        summary["pack"],
        json!({"name":"Fixture Pack","provider":"modrinth","projectId":"pack","versionId":"v1",
            "minecraftVersion":"26.1","loader":"fabric","issues":[]})
    );
    assert!(summary["restore"].is_null());
    assert_eq!(summary["runtime"]["automatic"], true);

    let (status, accepted) = host.create_apply(&job, "modpack-key-1", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["jobId"].as_str().unwrap().to_owned();
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    // Base software once, then the reviewed pack's content, then registration.
    assert_eq!(installer.installs.load(Ordering::SeqCst), 1);
    let installed = installer.pack_installs.lock().unwrap().clone();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].name, "Fixture Pack");
    let game = host.temp.path().join("servers").join(&server).join("game");
    assert_eq!(
        std::fs::read(game.join("config/fixture-pack.txt")).unwrap(),
        b"pack content"
    );
    assert!(
        std::fs::read_to_string(game.join("eula.txt"))
            .unwrap()
            .contains("eula=true")
    );
    let card = host
        .servers()
        .into_iter()
        .find(|s| s["serverId"] == server)
        .unwrap();
    assert_eq!(
        card["software"],
        json!({"kind":"fabric","version":"0.16.9"})
    );
    assert_eq!(card["minecraftVersion"], "26.1");

    let (status, started) = host.job_action(&id, "start");
    assert_eq!(status, 202, "{started}");
    let online = host.create_wait(&id, Duration::from_secs(90), |j| {
        matches!(j["state"].as_str(), Some("online" | "ready" | "failed"))
    });
    assert_eq!(online["state"], "online", "{online}");
    // Stop the fixture game before the host shuts down.
    let (status, view) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/server/status"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{view}");
    let data = if view["data"].is_object() {
        &view["data"]
    } else {
        &view
    };
    let generation = data["generation"].as_str().expect("generation").to_owned();
    let (status, stopped) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/lifecycle/stop"),
        json!({"expected_generation":generation}),
        &format!(
            "{}\r\nIdempotency-Key: modpack-lifecycle-stop-0001",
            host.owner
        ),
    );
    assert!((200..300).contains(&status), "{status} {stopped}");
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while host
        .servers()
        .iter()
        .any(|s| s["serverId"] == server && s["state"] != "stopped")
    {
        assert!(std::time::Instant::now() < until, "not stopped");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn client_only_and_unreadable_packs_are_blocked() {
    let installer = Arc::new(FakeInstaller::default());
    let host = create_host(installer.clone());
    let job = host.create_plan(modpack_body("Client", "client-only", 25620));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["pack_client_only"]));
    assert!(job["review"]["confirmation"].is_null());
    assert_eq!(job["review"]["summary"]["pack"]["name"], "Fixture Pack");

    let job = host.create_plan(modpack_body("Offline", "offline", 25621));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["provider_unavailable"]));
    assert!(job["review"]["summary"]["pack"]["name"].is_null());
    assert!(installer.pack_installs.lock().unwrap().is_empty());
}

#[test]
fn pack_review_issues_are_surfaced_verbatim() {
    let installer = Arc::new(FakeInstaller::default());
    let host = create_host(installer.clone());
    let job = host.create_plan(modpack_body("Notices", "notices", 25622));
    assert_eq!(job["state"], "review", "{job}");
    assert_eq!(job["review"]["warnings"], json!(["pack_review_warning"]));
    assert_eq!(
        job["review"]["summary"]["pack"]["issues"],
        json!([{"code":"pack_size_warning","message":"pack_size_warning:2048","blocking":false}])
    );
    // The owner must acknowledge the pack notices.
    let (status, error) = host.create_apply(&job, "notice-key", json!({"acknowledgedWarnings":[]}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("warnings_unacknowledged"))
    );

    let job = host.create_plan(modpack_body("Blocking", "blocking", 25623));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["pack_review_blocked"]));
    assert_eq!(
        job["review"]["summary"]["pack"]["issues"][0],
        json!({"code":"pack_path_invalid","message":"executable_directory: start.sh","blocking":true})
    );
}

#[test]
fn uploaded_packs_are_owner_bound_checked_and_expire() {
    let installer = Arc::new(FakeInstaller::default());
    let host = create_host(installer.clone());
    let pack = zip_bytes(&[
        ("modrinth.index.json", b"{}"),
        ("overrides/config/a.txt", b"a"),
    ]);
    // Owner only, raw octet-stream with a known kind.
    assert_eq!(host.upload(&host.stranger, "modpack", &pack).0, 403);
    assert_eq!(host.upload(&host.owner, "other", &pack).0, 400);
    assert_eq!(host.upload(&host.owner, "modpack", b"").0, 400);
    let (status, _, _) = host.wire().raw(
        "POST",
        "/v1/host/creates/uploads",
        &format!(
            "Content-Type: application/json\r\nX-Upload-Kind: modpack\r\nAuthorization: {}\r\n",
            host.owner
        ),
        &pack,
    );
    assert_eq!(status, 400);

    let (status, accepted) = host.upload(&host.owner, "modpack", &pack);
    assert_eq!(status, 201, "{accepted}");
    let upload = accepted["uploadId"].as_str().unwrap().to_owned();
    assert!(accepted["expiresAt"].as_str().unwrap().ends_with('Z'));
    let body = |name: &str, port: u16, id: &str| {
        json!({"kind":"modpack","name":name,"source":{"kind":"upload","uploadId":id},
            "gamePort":port,"memoryMiB":1024,"eulaAccepted":false})
    };
    let job = host.create_plan(body("Uploaded", 25630, &upload));
    assert_eq!(job["state"], "review", "{job}");
    let pack_summary = &job["review"]["summary"]["pack"];
    assert_eq!(pack_summary["name"], "Uploaded Pack");
    assert!(pack_summary["provider"].is_null() && pack_summary["projectId"].is_null());
    // Unknown upload ids are refused.
    let (status, _) = host.call(
        "POST",
        "/v1/host/creates/plan",
        body("Unknown", 25631, &Uuid::new_v4().to_string()),
        &host.owner,
    );
    assert_eq!(status, 400);

    // Unsafe archives (traversal) are refused at review.
    let (_, unsafe_upload) = host.upload(
        &host.owner,
        "modpack",
        &zip_bytes(&[("modrinth.index.json", b"{}"), ("../escape.txt", b"x")]),
    );
    let job = host.create_plan(body(
        "Unsafe",
        25632,
        unsafe_upload["uploadId"].as_str().unwrap(),
    ));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["unsafe_archive"]));
    // Not a zip at all.
    let (_, junk) = host.upload(&host.owner, "modpack", b"not a zip");
    let job = host.create_plan(body("Junk", 25633, junk["uploadId"].as_str().unwrap()));
    assert_eq!(job["review"]["blockers"], json!(["unsafe_archive"]));

    // An upload that expires after review fails apply without registering.
    let reviewed = host.create_plan(body("Expiring", 25634, &upload));
    assert_eq!(reviewed["state"], "review", "{reviewed}");
    host.state
        .write(|c| {
            c.execute("UPDATE machine_create_uploads SET expires_ms=0", [])?;
            Ok(())
        })
        .unwrap();
    let (status, accepted) = host.create_apply(&reviewed, "expiring-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let failed = host.create_wait(
        accepted["jobId"].as_str().unwrap(),
        Duration::from_secs(60),
        |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        },
    );
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["error"], "upload_unavailable");
    let server = accepted["serverId"].as_str().unwrap();
    assert!(!host.temp.path().join("servers").join(server).exists());
    assert!(host.servers().iter().all(|s| s["serverId"] != server));
    // Expired uploads cannot be planned again.
    let (status, _) = host.call(
        "POST",
        "/v1/host/creates/plan",
        body("Again", 25635, &upload),
        &host.owner,
    );
    assert_eq!(status, 400);
    assert!(installer.pack_installs.lock().unwrap().is_empty());
}

#[test]
fn oversized_uploads_are_refused_before_reading_the_body() {
    use std::io::{BufRead, Write};
    let host = create_host(Arc::new(FakeInstaller::default()));
    let mut wire = host.wire();
    write!(
        wire.0.get_mut(),
        "POST /v1/host/creates/uploads HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/octet-stream\r\nX-Upload-Kind: modpack\r\nAuthorization: {}\r\nContent-Length: {}\r\n\r\n",
        host.owner,
        vesta_control::machine::imports::jobs::UPLOAD_MAX_BYTES + 1
    )
    .unwrap();
    wire.0.get_mut().flush().unwrap();
    let mut line = String::new();
    wire.0.read_line(&mut line).unwrap();
    assert!(line.contains(" 413 "), "{line}");
}

#[test]
fn modpack_routes_are_owner_only() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let (status, value) = host.call(
        "POST",
        "/v1/host/creates/plan",
        modpack_body("Stranger", "pack", 25640),
        &host.stranger,
    );
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (403, json!("forbidden"))
    );
    // A planned modpack job is not visible to other devices.
    let job = host.create_plan(modpack_body("Mine", "pack", 25641));
    let id = job["jobId"].as_str().unwrap();
    let (status, _) = host.call(
        "GET",
        &format!("/v1/host/creates/jobs/{id}"),
        Value::Null,
        &host.stranger,
    );
    assert_eq!(status, 403);
}

/// An uploaded `.mrpack` with only overrides (nothing to download).
fn mrpack(dependencies: Value, files: Value) -> Vec<u8> {
    let index = serde_json::to_vec(&json!({
        "formatVersion": 1, "game": "minecraft", "versionId": "1.0", "name": "Local Pack",
        "dependencies": dependencies, "files": files,
    }))
    .unwrap();
    zip_bytes(&[
        ("modrinth.index.json", &index),
        ("overrides/config/local-pack.txt", b"from the pack"),
    ])
}
fn upload_body(name: &str, port: u16, upload: &Value) -> Value {
    json!({"kind":"modpack","name":name,"source":{"kind":"upload","uploadId":upload["uploadId"]},
        "gamePort":port,"memoryMiB":1024,"eulaAccepted":false})
}
/// The created server's own software status (`manage/software`).
fn software_status(host: &Host, server: &str) -> Value {
    // Registered servers get their worker on first use (start).
    host.router
        .import_worker(server.parse().unwrap())
        .expect("worker");
    let (status, view) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/software"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{view}");
    let software = if view["data"].is_object() { view["data"].clone() } else { view };
    let (status, inventory) = host.call("GET", &format!("/v1/servers/{server}/manage/content"), Value::Null, &host.owner);
    assert_eq!(status, 200, "{inventory}");
    let inventory = if inventory["data"].is_object() { &inventory["data"] } else { &inventory };
    assert_eq!(inventory["software"], software["current"]["software"], "{inventory}");
    software
}

/// Loader-less (Vanilla) and loader packs through the live pack engine: the
/// base software is installed exactly once (by the create), recorded as the
/// server's software, and the pack install adds only the pack's files.
#[test]
fn modpack_creates_install_software_once_for_vanilla_and_loader_packs() {
    let installer = Arc::new(FakeInstaller::default());
    installer.live_packs.store(true, Ordering::SeqCst);
    let host = create_host(installer.clone());
    for (name, dependencies, kind, loader) in [
        ("Plain Pack", json!({"minecraft":"26.1"}), "vanilla", None),
        (
            "Fabric Pack",
            json!({"minecraft":"26.1","fabric-loader":"0.16.9"}),
            "fabric",
            Some("0.16.9"),
        ),
    ] {
        let before = installer.installs.load(Ordering::SeqCst);
        let (status, upload) =
            host.upload(&host.owner, "modpack", &mrpack(dependencies, json!([])));
        assert_eq!(status, 201, "{upload}");
        let job = host.create_plan(upload_body(name, free_port(), &upload));
        assert_eq!(job["state"], "review", "{job}");
        let summary = &job["review"]["summary"];
        assert_eq!(summary["minecraftVersion"], "26.1");
        assert_eq!(summary["software"]["kind"], kind);
        assert_eq!(summary["software"]["loaderVersion"], json!(loader));
        assert_eq!(summary["pack"]["loader"], kind);
        let (status, accepted) = host.create_apply(&job, &format!("once-{kind}"), json!({}));
        assert_eq!(status, 202, "{accepted}");
        let id = accepted["jobId"].as_str().unwrap();
        let ready = host.create_wait(id, Duration::from_secs(120), |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        });
        assert_eq!(ready["state"], "online", "{ready}");
        // One software install per create: the base install, nothing else.
        assert_eq!(installer.installs.load(Ordering::SeqCst), before + 1);
        let server = accepted["serverId"].as_str().unwrap();
        let game = host.temp.path().join("servers").join(server).join("game");
        assert_eq!(
            std::fs::read(game.join("config/local-pack.txt")).unwrap(),
            b"from the pack"
        );
        // The software service recorded the create's install and never
        // activated another one (revision 0, no previous installation).
        let software = software_status(&host, server);
        assert_eq!(software["revision"], 0, "{software}");
        assert!(software["previous"].is_null(), "{software}");
        assert_eq!(software["current"]["software"], kind, "{software}");
        assert_eq!(software["current"]["version"], "26.1", "{software}");
        assert_eq!(
            software["current"]["loader_version"],
            json!(loader),
            "{software}"
        );
        let card = host
            .servers()
            .into_iter()
            .find(|s| s["serverId"] == server)
            .unwrap();
        assert_eq!(card["software"]["kind"], kind);
    }
    assert_eq!(installer.pack_installs.lock().unwrap().len(), 2);
}

/// A loader-less pack that lists mods could only run on Vanilla, which cannot
/// load them: refused at planning, never at apply.
#[test]
fn loader_less_packs_with_mods_are_blocked_at_planning() {
    let installer = Arc::new(FakeInstaller::default());
    installer.live_packs.store(true, Ordering::SeqCst);
    let host = create_host(installer.clone());
    let files = json!([{"path":"mods/a.jar","hashes":{"sha1":"a","sha512":"b"},
        "env":{"client":"required","server":"required"},
        "downloads":["https://cdn.modrinth.com/data/x/a.jar"],"fileSize":1}]);
    let (_, upload) = host.upload(
        &host.owner,
        "modpack",
        &mrpack(json!({"minecraft":"26.1"}), files),
    );
    let job = host.create_plan(upload_body("Mods", 25650, &upload));
    assert_eq!(job["state"], "blocked", "{job}");
    assert_eq!(job["review"]["blockers"], json!(["pack_loader_missing"]));
    assert!(job["review"]["confirmation"].is_null());
    assert_eq!(installer.installs.load(Ordering::SeqCst), 0);
}

/// Cancel while the pack content installs: the live pack install stops at
/// its next safe boundary, the job settles cancelled, nothing is registered
/// and the reservation and destination are released.
#[test]
fn cancel_during_pack_install_releases_everything() {
    let installer = Arc::new(FakeInstaller::default());
    installer.live_packs.store(true, Ordering::SeqCst);
    installer.pack_hold.store(true, Ordering::SeqCst);
    let host = create_host(installer.clone());
    let (_, upload) = host.upload(
        &host.owner,
        "modpack",
        &mrpack(json!({"minecraft":"26.1"}), json!([])),
    );
    let job = host.create_plan(upload_body("Halted", 25651, &upload));
    assert_eq!(job["state"], "review", "{job}");
    let (status, accepted) = host.create_apply(&job, "halted-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["jobId"].as_str().unwrap().to_owned();
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    // The base software is installed; the pack install is under way.
    host.create_wait(&id, Duration::from_secs(60), |j| {
        j["phase"] == "installing" && installer.pack_started.load(Ordering::SeqCst)
    });
    assert!(
        host.temp
            .path()
            .join("servers")
            .join(&server)
            .join("game")
            .exists()
    );
    let (status, requested) = host.job_action(&id, "cancel");
    assert_eq!((status, requested), (200, json!({"state":"applying"})));
    let done = host.create_wait(&id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(done["state"], "cancelled", "{done}");
    assert!(!host.temp.path().join("servers").join(&server).exists());
    assert!(host.servers().is_empty());
    assert!(installer.pack_installs.lock().unwrap().is_empty());
    // Released: the same name and port plan cleanly again.
    installer.pack_hold.store(false, Ordering::SeqCst);
    let again = host.create_plan(upload_body("Halted", 25651, &upload));
    assert_eq!(again["state"], "review", "{again}");
}

/// A small real Modrinth Fabric server pack ("Smooth Server", Minecraft
/// 1.21.11) created through the router with the live installer and the
/// server's own pack engine, then started until online and stopped.
#[test]
#[ignore = "downloads Java, Fabric and a real Modrinth pack, then runs the server"]
fn real_modrinth_fabric_modpack_create_reaches_online() {
    let scratch = tempfile::tempdir().unwrap();
    let java_cache = scratch.path().join("java");
    std::fs::create_dir_all(&java_cache).unwrap();
    let env = Arc::new(WindowsSafeEnv(HostEnvironment {
        java_cache: java_cache.clone(),
    }));
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("servers")).unwrap();
    let servers = std::fs::canonicalize(temp.path().join("servers")).unwrap();
    let installer = Arc::new(vesta_control::machine::creates::LiveCreateInstaller::new(
        java_cache, servers,
    ));
    let live =
        Arc::new(vesta_control::machine::creates::LiveCatalogue::open(scratch.path()).unwrap());
    let host = Host::start(temp, env, Some(installer), None, Some(live));
    let port = free_port();
    let job = host.create_plan(json!({"kind":"modpack","name":"Smooth Real",
        "source":{"kind":"provider","provider":"modrinth","projectId":"NnAGuoCJ","versionId":"sYyD6ODr"},
        "gamePort":port,"memoryMiB":2048,"eulaAccepted":false}));
    assert_eq!(job["state"], "review", "{job}");
    let summary = &job["review"]["summary"];
    assert_eq!(summary["kind"], "modpack");
    assert_eq!(summary["minecraftVersion"], "1.21.11");
    assert_eq!(summary["software"]["kind"], "fabric");
    assert_eq!(summary["pack"]["minecraftVersion"], "1.21.11");
    assert_eq!(summary["pack"]["loader"], "fabric");
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "real-modpack", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    let dir = host.temp.path().join("servers").join(&server);
    let ready = host.create_wait(&id, Duration::from_secs(1800), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    let mods: Vec<_> = std::fs::read_dir(dir.join("game/mods"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!mods.is_empty(), "pack mods installed");
    let (status, started) = host.job_action(&id, "start");
    assert_eq!(status, 202, "{started}");
    let online = host.create_wait(&id, Duration::from_secs(600), |j| j["state"] != "starting");
    if online["state"] != "online" {
        eprintln!(
            "launch.toml:\n{}\nlatest.log:\n{}",
            std::fs::read_to_string(dir.join("private/launch.toml")).unwrap_or_default(),
            std::fs::read_to_string(dir.join("game/logs/latest.log")).unwrap_or_default()
        );
    }
    assert_eq!(online["state"], "online", "{online}");
    let card = host
        .servers()
        .into_iter()
        .find(|s| s["serverId"] == server)
        .unwrap();
    assert_eq!(card["software"]["kind"], "fabric");
    let (status, view) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/server/status"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{view}");
    let data = if view["data"].is_object() {
        &view["data"]
    } else {
        &view
    };
    let generation = data["generation"].as_str().unwrap().to_owned();
    let (status, stopped) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/lifecycle/stop"),
        json!({"expected_generation":generation}),
        &format!("{}\r\nIdempotency-Key: real-modpack-stop-0001", host.owner),
    );
    assert!((200..300).contains(&status), "{status} {stopped}");
    let until = std::time::Instant::now() + Duration::from_secs(180);
    while host.servers().iter().any(|s| s["state"] != "stopped") {
        assert!(std::time::Instant::now() < until, "not stopped");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The caller's own projection on one server under the host model
/// (`manage/session`), from the trusted server subject: owners get every
/// grantable capability, server roles exactly their role's grants, and a
/// caller without a role on the server gets 404.
#[test]
fn server_session_projects_the_callers_roles_and_capabilities() {
    use vesta_control::identity::BuiltinRole;
    use vesta_control::machine::owners::{Actor, ServerRole as HostServerRole};
    let host = create_host(Arc::new(FakeInstaller::default()));
    let job = host.create_plan(new_body("Projected", Some(25660), Some(1024)));
    let (status, accepted) = host.create_apply(&job, "projected-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["jobId"].as_str().unwrap();
    let ready = host.create_wait(id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    let server: Uuid = accepted["serverId"].as_str().unwrap().parse().unwrap();
    host.router.import_worker(server).expect("worker");
    let path = format!("/v1/servers/{server}/manage/session");
    let expected = |role: BuiltinRole| -> Vec<String> {
        let mut caps: Vec<String> = role
            .capabilities()
            .iter()
            .filter(|c| !c.local_only())
            .map(|c| c.id().to_owned())
            // Identity administration belongs to the host, never to a server.
            .filter(|c| {
                ![
                    "roles.create",
                    "roles.manage",
                    "devices.",
                    "credentials.",
                    "security.",
                ]
                .iter()
                .any(|p| c.starts_with(p))
            })
            .filter(|c| c != "access-sources.manage" || role == BuiltinRole::Owner)
            .collect();
        caps.sort();
        caps
    };
    let check = |auth: &str, role: BuiltinRole| {
        let (status, view) = host.call("GET", &path, Value::Null, auth);
        assert_eq!(status, 200, "{view}");
        let view = if view["data"].is_object() {
            view["data"].clone()
        } else {
            view
        };
        assert_eq!(view["server_id"], json!(server), "{view}");
        assert!(
            view["principal_id"].as_str().is_some_and(|p| !p.is_empty()),
            "{view}"
        );
        let roles: Vec<&str> = view["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert!(roles.contains(&role.id()), "{view}");
        let mut caps: Vec<String> = view["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_owned())
            .collect();
        caps.sort();
        assert_eq!(caps, expected(role), "{view}");
    };
    check(&host.owner, BuiltinRole::Owner);
    let storage_path = format!("/v1/servers/{server}/manage/storage");
    let root = host.router.imports().unwrap().servers_root().join(server.to_string());
    let folder = root.join("game/world/probe");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("marker"), vec![0; 32]).unwrap();
    host.router.storage.refresh();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let (status, reply) = host.call("GET", &storage_path, Value::Null, &host.owner);
        assert_eq!(status, 200, "{reply}");
        if reply["data"]["largest"].as_array().unwrap().iter().any(|entry| {
            entry == &json!({"path":"world/probe","bytes":32})
        }) {
            assert!(reply["data"]["volume"]["totalBytes"].is_number());
            assert!(reply["data"]["largest"].as_array().unwrap().len() <= 10);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{reply}");
        std::thread::sleep(Duration::from_millis(150));
    }
    assert_eq!(host.call("GET", &storage_path, Value::Null, &host.stranger).0, 404);
    // No role on this server: the server does not exist for this caller.
    let (status, _) = host.call("GET", &path, Value::Null, &host.stranger);
    assert_eq!(status, 404);
    let stranger = host
        .state
        .device(host.devices.1)
        .unwrap()
        .unwrap()
        .account_id;
    for (role, builtin) in [
        (HostServerRole::Admin, BuiltinRole::Administrator),
        (HostServerRole::Player, BuiltinRole::PackUser),
    ] {
        let revision = host.state.snapshot().unwrap().revision;
        host.state
            .set_server_role(Actor::LocalRecovery, server, stranger, Some(role), revision)
            .unwrap();
        check(&host.stranger, builtin);
        let (status, reply) = host.call("GET", &storage_path, Value::Null, &host.stranger);
        assert_eq!(status, if role == HostServerRole::Admin { 200 } else { 403 }, "{reply}");
    }
    let previous = host.state.snapshot().unwrap();
    let device = host.state.device(host.devices.1).unwrap().unwrap();
    let stale = vesta_control::worker::ServerSubject {
        host_id: previous.host_id,
        server_id: server,
        account_id: stranger,
        device_id: device.device_id,
        generation: previous.auth_generation,
        issued_at_ms: host.state.now_ms(),
        expires_at_ms: host.state.now_ms() + 30000,
        capabilities: vec!["status.view".into()],
    };
    let (status, users) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/users"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{users}");
    let user = users["data"]["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == stranger.to_string())
        .unwrap();
    let (status, revoked) = host.call(
        "PUT",
        &format!("/v1/servers/{server}/manage/users/{stranger}/roles"),
        json!({"add":[],"remove":["player"],"expected_version":user["version"]}),
        &host.owner,
    );
    assert_eq!(status, 200, "{revoked}");
    assert!(host.state.snapshot().unwrap().auth_generation > previous.auth_generation);
    assert_eq!(host.call("GET", &path, Value::Null, &host.stranger).0, 404);
    assert!(
        host.router
            .owned_handle(server)
            .unwrap()
            .authorize(stale)
            .is_err(),
        "old worker subject survived revocation"
    );
    // Only GET, with no query or body.
    assert_eq!(host.call("POST", &path, json!({}), &host.owner).0, 400);
}

/// A Mojang-style `server.jar` declaring its own version.
fn version_jar(version: &str) -> Vec<u8> {
    zip_bytes(&[(
        "version.json",
        format!("{{\"id\":\"{version}\"}}").as_bytes(),
    )])
}
/// Back up `<dir>/game` into the folder's default repository as a promoted
/// restore point, as the server's backup service does.
fn backup_folder(dir: &Path) -> String {
    use vesta_control::backups::{engine::Engine, ignore::Ignore, store::LocalStore};
    let engine = Engine::new(Box::new(LocalStore::new(dir.join("backups/repository"))));
    engine.init().unwrap();
    engine
        .backup(
            &dir.join("game"),
            &Ignore::parse("").unwrap(),
            &["vesta".into(), "profile=manual".into()],
            None,
        )
        .unwrap()
        .snapshot_id
}
/// A server folder the registry does not know, holding one backup of a
/// Vanilla game (world, properties, its own jar).
fn detached_folder(host: &Host) -> (Uuid, String) {
    let origin = Uuid::new_v4();
    let dir = host.temp.path().join("servers").join(origin.to_string());
    let game = dir.join("game");
    std::fs::create_dir_all(game.join("world/region")).unwrap();
    std::fs::write(game.join("world/level.dat"), b"level").unwrap();
    std::fs::write(
        game.join("world/region/r.0.0.mca"),
        vec![7u8; 3 * 1024 * 1024],
    )
    .unwrap();
    std::fs::write(
        game.join("server.properties"),
        "server-port=25599\nmotd=Old\n",
    )
    .unwrap();
    std::fs::write(game.join("server.jar"), version_jar("26.1")).unwrap();
    let snapshot = backup_folder(&dir);
    (origin, snapshot)
}
fn restore_body(name: &str, source: &str, port: u16) -> Value {
    json!({"kind":"restore","name":name,"source":{"kind":"snapshot","sourceId":source},
        "gamePort":port,"memoryMiB":1024,"eulaAccepted":false})
}
fn restore_sources(host: &Host) -> Vec<Value> {
    let (status, list) = host.call(
        "GET",
        "/v1/host/creates/restore-sources",
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{list}");
    list["sources"].as_array().unwrap().clone()
}

/// Restore sources are backups of deleted servers and of folders no server
/// owns, never a live server's; a restore plans with the known facts (or
/// warns when unknown), restores the verified snapshot as a new server and
/// keeps the source.
#[test]
fn restore_lists_detached_backups_and_restores_one_as_a_new_server() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    // A live server with its own backup: never a restore source.
    let job = host.create_plan(new_body("Live", Some(25670), Some(1024)));
    let (status, accepted) = host.create_apply(&job, "live-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let ready = host.create_wait(
        accepted["jobId"].as_str().unwrap(),
        Duration::from_secs(60),
        |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        },
    );
    assert_eq!(ready["state"], "online", "{ready}");
    let live: Uuid = accepted["serverId"].as_str().unwrap().parse().unwrap();
    let live_dir = host.temp.path().join("servers").join(live.to_string());
    std::fs::write(live_dir.join("game/server.jar"), version_jar("26.1")).unwrap();
    let live_snapshot = backup_folder(&live_dir);
    assert!(restore_sources(&host).is_empty());

    // An unknown folder: listed without facts.
    let (origin, snapshot) = detached_folder(&host);
    let sources = restore_sources(&host);
    assert_eq!(sources.len(), 1, "{sources:?}");
    let detached = sources[0].clone();
    assert_eq!(
        detached["sourceId"],
        format!("{}{snapshot}", origin.simple())
    );
    assert_eq!(detached["label"], "Recovered server");
    assert!(detached["minecraftVersion"].is_null() && detached["software"].is_null());
    assert!(detached["bytes"].as_u64().unwrap() > 3 * 1024 * 1024);
    assert!(detached["createdAt"].as_str().unwrap().ends_with('Z'));

    // Deleting the live server makes its backup a source, with its facts.
    let owner = host
        .state
        .device(host.devices.0)
        .unwrap()
        .unwrap()
        .account_id;
    let mut entry = host.state.server(live).unwrap().unwrap();
    entry.state = vesta_control::machine::registry::ServerState::Deleted;
    let revision = host.state.snapshot().unwrap().revision;
    host.state
        .update_server(
            vesta_control::machine::owners::Actor::Account(owner),
            &entry,
            revision,
        )
        .unwrap();
    let sources = restore_sources(&host);
    let deleted = sources
        .iter()
        .find(|s| s["sourceId"] == format!("{}{live_snapshot}", live.simple()))
        .expect("deleted server's backup")
        .clone();
    assert_eq!(deleted["label"], "Live");
    assert_eq!(deleted["minecraftVersion"], "26.1");
    assert_eq!(
        deleted["software"],
        json!({"kind":"vanilla","version":"26.1"})
    );
    let known = host.create_plan(restore_body(
        "Back",
        deleted["sourceId"].as_str().unwrap(),
        25671,
    ));
    assert_eq!(known["state"], "review", "{known}");
    assert_eq!(known["review"]["warnings"], json!([]));
    assert_eq!(known["review"]["summary"]["software"]["kind"], "vanilla");
    assert_eq!(known["review"]["summary"]["minecraftVersion"], "26.1");
    assert_eq!(
        host.job_action(known["jobId"].as_str().unwrap(), "cancel")
            .0,
        200
    );

    // Restore the unknown folder's backup.
    let job = host.create_plan(restore_body(
        "Restored",
        detached["sourceId"].as_str().unwrap(),
        25672,
    ));
    assert_eq!(job["state"], "review", "{job}");
    let review = &job["review"];
    assert_eq!(review["warnings"], json!(["restore_version_unknown"]));
    assert_eq!(review["summary"]["kind"], "restore");
    assert_eq!(review["summary"]["minecraftVersion"], "");
    assert_eq!(review["summary"]["software"]["kind"], "unknown");
    assert_eq!(
        review["summary"]["restore"],
        json!({"sourceId":detached["sourceId"],"label":"Recovered server","createdAt":detached["createdAt"],"installSoftware":false})
    );
    assert!(review["summary"]["pack"].is_null());
    let (status, error) =
        host.create_apply(&job, "restore-key", json!({"acknowledgedWarnings":[]}));
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (400, json!("warnings_unacknowledged"))
    );
    let (status, accepted) = host.create_apply(&job, "restore-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["jobId"].as_str().unwrap();
    let ready = host.create_wait(id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["error"], "start_failed");
    let server = accepted["serverId"].as_str().unwrap();
    let dir = host.temp.path().join("servers").join(server);
    assert_eq!(
        std::fs::read(dir.join("game/world/level.dat")).unwrap(),
        b"level"
    );
    assert_eq!(
        std::fs::read(dir.join("game/world/region/r.0.0.mca"))
            .unwrap()
            .len(),
        3 * 1024 * 1024
    );
    let properties = std::fs::read_to_string(dir.join("game/server.properties")).unwrap();
    assert!(
        properties.contains(&format!("server-port={}", test_port(25672)))
            && properties.contains("motd=Old"),
        "{properties}"
    );
    assert!(
        std::fs::read_to_string(dir.join("game/eula.txt"))
            .unwrap()
            .contains("eula=true")
    );
    let launch = std::fs::read_to_string(dir.join("private/launch.toml")).unwrap();
    assert!(
        launch.contains("server.jar") && launch.contains("nogui"),
        "{launch}"
    );
    assert!(!dir.join("staging").exists());
    let card = host
        .servers()
        .into_iter()
        .find(|s| s["serverId"] == server)
        .unwrap();
    assert_eq!(card["name"], "Restored");
    assert_eq!(card["gamePort"], test_port(25672));
    // The source stays, and is still offered (a backup can be restored again).
    let source_dir = host.temp.path().join("servers").join(origin.to_string());
    assert!(source_dir.join("game/world/level.dat").exists());
    assert!(
        restore_sources(&host)
            .iter()
            .any(|s| s["sourceId"] == detached["sourceId"])
    );
    // The restored server is live: its own backups are never listed.
    let restored: Uuid = server.parse().unwrap();
    backup_folder(&dir);
    let prefix = restored.simple().to_string();
    assert!(
        restore_sources(&host)
            .iter()
            .all(|s| !s["sourceId"].as_str().unwrap().starts_with(&prefix))
    );
}

/// Mark a registered server deleted (its folder and backups remain).
fn delete_server(host: &Host, server: Uuid) {
    let owner = host
        .state
        .device(host.devices.0)
        .unwrap()
        .unwrap()
        .account_id;
    let mut entry = host.state.server(server).unwrap().unwrap();
    entry.state = vesta_control::machine::registry::ServerState::Deleted;
    let revision = host.state.snapshot().unwrap().revision;
    host.state
        .update_server(
            vesta_control::machine::owners::Actor::Account(owner),
            &entry,
            revision,
        )
        .unwrap();
}

/// A server Vesta created keeps its software under `<server>/install`, which
/// its backups do not cover (exactly like the live installer's layout): the
/// game directory alone has no server to run. Restoring such a backup
/// installs the recorded software through the same installer as `new`,
/// publishes the verified snapshot as the game directory and reaches online.
#[test]
fn restore_of_a_created_server_reinstalls_its_software_and_reaches_online() {
    let installer = Arc::new(FakeInstaller::default());
    let host = create_host(installer.clone());
    let job = host.create_plan(new_body("Original", Some(25710), Some(1024)));
    let (status, accepted) = host.create_apply(&job, "original-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let ready = host.create_wait(
        accepted["jobId"].as_str().unwrap(),
        Duration::from_secs(60),
        |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        },
    );
    assert_eq!(ready["state"], "online", "{ready}");
    let origin: Uuid = accepted["serverId"].as_str().unwrap().parse().unwrap();
    let dir = host.temp.path().join("servers").join(origin.to_string());
    // The real layout: the software lives in install/, game/ has no jar.
    assert!(dir.join("install/software").is_dir());
    assert!(
        std::fs::read_dir(dir.join("game"))
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".jar"))
    );
    std::fs::create_dir_all(dir.join("game/world")).unwrap();
    std::fs::write(dir.join("game/world/level.dat"), b"original world").unwrap();
    std::fs::write(
        dir.join("game/server.properties"),
        "server-port=25710\nmotd=Kept\n",
    )
    .unwrap();
    let snapshot = backup_folder(&dir);
    delete_server(&host, origin);
    let source = restore_sources(&host)
        .into_iter()
        .find(|s| s["sourceId"] == format!("{}{snapshot}", origin.simple()))
        .expect("the deleted server's backup");

    let job = host.create_plan(restore_body(
        "Reborn",
        source["sourceId"].as_str().unwrap(),
        25711,
    ));
    assert_eq!(job["state"], "review", "{job}");
    let summary = &job["review"]["summary"];
    assert_eq!(summary["kind"], "restore");
    assert_eq!(summary["minecraftVersion"], "26.1");
    // The review says exactly what will be installed.
    assert_eq!(
        summary["software"],
        json!({"kind":"vanilla","version":"26.1","build":null,"loaderVersion":null})
    );
    assert_eq!(summary["runtime"]["requiredMajor"], 21);
    assert_eq!(
        summary["restore"],
        json!({"sourceId":source["sourceId"],"label":"Original","createdAt":source["createdAt"],"installSoftware":true})
    );
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "reborn-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    assert_eq!(installer.installs.load(Ordering::SeqCst), 2);
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    let dir = host.temp.path().join("servers").join(&server);
    assert!(dir.join("install/software").is_dir());
    assert!(!dir.join("staging").exists());
    assert_eq!(
        std::fs::read(dir.join("game/world/level.dat")).unwrap(),
        b"original world"
    );
    let properties = std::fs::read_to_string(dir.join("game/server.properties")).unwrap();
    assert!(
        properties.contains(&format!("server-port={}", test_port(25711)))
            && properties.contains("motd=Kept"),
        "{properties}"
    );
    assert!(
        std::fs::read_to_string(dir.join("game/eula.txt"))
            .unwrap()
            .contains("eula=true")
    );
    assert_eq!(host.job_action(&id, "start").0, 202);
    let online = host.create_wait(&id, Duration::from_secs(60), |j| j["state"] != "starting");
    assert_eq!(online["state"], "online", "{online}");
    let card = host
        .servers()
        .into_iter()
        .find(|s| s["serverId"] == server.as_str())
        .unwrap();
    assert_eq!(card["software"]["kind"], "vanilla");
    assert_eq!(card["minecraftVersion"], "26.1");
    stop_server(&host, &server, "reborn-stop-0001");
}

/// A snapshot whose stored data no longer verifies is refused: the job fails
/// with `integrity_conflict`, nothing is published or registered.
#[test]
fn corrupted_snapshots_are_refused_before_anything_is_published() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    let (origin, _) = detached_folder(&host);
    let source = restore_sources(&host)[0]["sourceId"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = host.create_plan(restore_body("Damaged", &source, 25680));
    assert_eq!(job["state"], "review", "{job}");
    // Flip the last byte of every stored chunk after the review.
    let chunks = host
        .temp
        .path()
        .join("servers")
        .join(origin.to_string())
        .join("backups/repository/chunks");
    for prefix in std::fs::read_dir(&chunks).unwrap().flatten() {
        for chunk in std::fs::read_dir(prefix.path()).unwrap().flatten() {
            let mut bytes = std::fs::read(chunk.path()).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
            std::fs::write(chunk.path(), bytes).unwrap();
        }
    }
    let (status, accepted) = host.create_apply(&job, "damaged-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let done = host.create_wait(
        accepted["jobId"].as_str().unwrap(),
        Duration::from_secs(60),
        |j| {
            !matches!(j["state"].as_str(), Some("applying" | "starting"))
                && (j["state"] != "ready" || !j["error"].is_null())
        },
    );
    assert_eq!(
        (done["state"].clone(), done["error"].clone()),
        (json!("failed"), json!("integrity_conflict")),
        "{done}"
    );
    let server = accepted["serverId"].as_str().unwrap();
    assert!(!host.temp.path().join("servers").join(server).exists());
    assert!(host.servers().is_empty());
}

/// Restore routes are owner-only; uploaded recovery exports are not a source
/// yet; unknown or malformed sources are refused.
#[test]
fn restore_routes_are_owner_only_and_uploads_are_not_supported_yet() {
    let host = create_host(Arc::new(FakeInstaller::default()));
    detached_folder(&host);
    let source = restore_sources(&host)[0]["sourceId"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, value) = host.call(
        "GET",
        "/v1/host/creates/restore-sources",
        Value::Null,
        &host.stranger,
    );
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (403, json!("forbidden"))
    );
    let (status, value) = host.call(
        "POST",
        "/v1/host/creates/plan",
        restore_body("Mine", &source, 25690),
        &host.stranger,
    );
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (403, json!("forbidden"))
    );
    let job = host.create_plan(restore_body("Mine", &source, 25690));
    let (status, _) = host.call(
        "GET",
        &format!("/v1/host/creates/jobs/{}", job["jobId"].as_str().unwrap()),
        Value::Null,
        &host.stranger,
    );
    assert_eq!(status, 403);
    let (status, value) = host.call(
        "POST",
        "/v1/host/creates/plan",
        json!({"kind":"restore","name":"Up","source":{"kind":"upload","uploadId":Uuid::new_v4()},"gamePort":25691,"memoryMiB":1024}),
        &host.owner,
    );
    assert_eq!(
        (status, value["error"]["code"].clone()),
        (501, json!("not_supported_yet"))
    );
    assert_eq!(host.upload(&host.owner, "restore", b"PK").0, 501);
    let (status, _) = host.call(
        "POST",
        "/v1/host/creates/plan",
        restore_body("Bad", "nothex", 25692),
        &host.owner,
    );
    assert_eq!(status, 400);
    // Well-formed but unknown: planned and blocked.
    let unknown = host.create_plan(restore_body("Gone", &"0".repeat(64), 25693));
    assert_eq!(unknown["state"], "blocked", "{unknown}");
    assert_eq!(
        unknown["review"]["blockers"],
        json!(["restore_source_unavailable"])
    );
}

/// Cancel while the restored server's Java is made available (after the
/// snapshot is staged) releases everything; a restore interrupted by a host
/// restart resumes from a clean destination to exactly one server.
#[test]
fn restore_cancel_and_resume_leave_no_duplicates() {
    let env = fake_env();
    env.java_gate.store(true, Ordering::SeqCst);
    let host = Host::start(
        tempfile::tempdir().unwrap(),
        env.clone(),
        Some(Arc::new(FakeInstaller::default())),
        None,
        None,
    );
    detached_folder(&host);
    let source = restore_sources(&host)[0]["sourceId"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = host.create_plan(restore_body("Paused", &source, 25700));
    let (status, accepted) = host.create_apply(&job, "paused-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["jobId"].as_str().unwrap().to_owned();
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    host.create_wait(&id, Duration::from_secs(30), |j| {
        j["phase"] == "downloading_java"
    });
    let (status, requested) = host.job_action(&id, "cancel");
    assert_eq!((status, requested), (200, json!({"state":"applying"})));
    env.java_gate.store(false, Ordering::SeqCst);
    let done = host.create_wait(&id, Duration::from_secs(30), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(done["state"], "cancelled", "{done}");
    assert!(!host.temp.path().join("servers").join(&server).exists());
    assert!(host.servers().is_empty());

    // Interrupted by a restart while waiting for Java, then resumed.
    env.java_gate.store(true, Ordering::SeqCst);
    let job = host.create_plan(restore_body("Paused", &source, 25700));
    assert_eq!(job["state"], "review", "{job}");
    let (status, accepted) = host.create_apply(&job, "resume-key", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["jobId"].as_str().unwrap().to_owned();
    host.create_wait(&id, Duration::from_secs(30), |j| {
        j["phase"] == "downloading_java"
    });
    let host = host.restart(fake_env(), Some(Arc::new(FakeInstaller::default())));
    let (_, interrupted) = host.call(
        "GET",
        &format!("/v1/host/creates/jobs/{id}"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(interrupted["state"], "interrupted", "{interrupted}");
    let (status, resumed) = host.job_action(&id, "resume");
    assert_eq!(status, 202, "{resumed}");
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["error"], "start_failed");
    let servers = host.servers();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["serverId"], accepted["serverId"]);
    let dir = host
        .temp
        .path()
        .join("servers")
        .join(accepted["serverId"].as_str().unwrap());
    assert!(dir.join("game/world/level.dat").exists() && !dir.join("staging").exists());
}

/// Stop a running server through its lifecycle route and wait until stopped.
fn stop_server(host: &Host, server: &str, key: &str) {
    let (status, view) = host.call(
        "GET",
        &format!("/v1/servers/{server}/manage/server/status"),
        Value::Null,
        &host.owner,
    );
    assert_eq!(status, 200, "{view}");
    let data = if view["data"].is_object() {
        &view["data"]
    } else {
        &view
    };
    let generation = data["generation"].as_str().unwrap().to_owned();
    let (status, stopped) = host.call(
        "POST",
        &format!("/v1/servers/{server}/manage/lifecycle/stop"),
        json!({"expected_generation":generation}),
        &format!("{}\r\nIdempotency-Key: {key}", host.owner),
    );
    assert!((200..300).contains(&status), "{status} {stopped}");
    let until = std::time::Instant::now() + Duration::from_secs(180);
    while host
        .servers()
        .iter()
        .any(|s| s["serverId"] == server && s["state"] != "stopped")
    {
        assert!(std::time::Instant::now() < until, "not stopped");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Real restore: create latest stable Vanilla, reach online, stop, back it up
/// into its own repository (the backup engine, as the server's backup service
/// writes it), delete the server through the registry, then restore that
/// backup as a new server through the router and reach online again.
#[test]
#[ignore = "downloads Java and Vanilla, runs the server twice"]
fn real_vanilla_backup_restores_as_a_new_server_and_reaches_online() {
    let scratch = tempfile::tempdir().unwrap();
    let java_cache = scratch.path().join("java");
    std::fs::create_dir_all(&java_cache).unwrap();
    let env = Arc::new(WindowsSafeEnv(HostEnvironment {
        java_cache: java_cache.clone(),
    }));
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("servers")).unwrap();
    let servers = std::fs::canonicalize(temp.path().join("servers")).unwrap();
    let installer = Arc::new(vesta_control::machine::creates::LiveCreateInstaller::new(
        java_cache, servers,
    ));
    let live =
        Arc::new(vesta_control::machine::creates::LiveCatalogue::open(scratch.path()).unwrap());
    let host = Host::start(temp, env, Some(installer), None, Some(live));
    let (_, catalogue) = host.call(
        "GET",
        "/v1/host/catalogue/minecraft",
        Value::Null,
        &host.owner,
    );
    let latest = catalogue["latestStable"].as_str().unwrap().to_owned();
    let job = host.create_plan(
        json!({"kind":"new","name":"Original","minecraftVersion":latest,
        "software":{"kind":"vanilla","build":null,"loaderVersion":null},
        "gamePort":free_port(),"memoryMiB":1024,"eulaAccepted":false}),
    );
    assert_eq!(job["state"], "review", "{job}");
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "real-original", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let original = accepted["serverId"].as_str().unwrap().to_owned();
    let ready = host.create_wait(&id, Duration::from_secs(900), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    assert_eq!(host.job_action(&id, "start").0, 202);
    let online = host.create_wait(&id, Duration::from_secs(600), |j| j["state"] != "starting");
    assert_eq!(online["state"], "online", "{online}");
    stop_server(&host, &original, "real-original-stop-0001");

    let dir = host.temp.path().join("servers").join(&original);
    assert!(
        dir.join("game/world/level.dat").is_file(),
        "world generated"
    );
    std::fs::write(
        dir.join("game/world/vesta-restore-proof.txt"),
        b"backed-up world",
    )
    .unwrap();
    let snapshot = backup_folder(&dir);
    let origin: Uuid = original.parse().unwrap();
    let owner = host
        .state
        .device(host.devices.0)
        .unwrap()
        .unwrap()
        .account_id;
    let mut entry = host.state.server(origin).unwrap().unwrap();
    entry.state = vesta_control::machine::registry::ServerState::Deleted;
    let revision = host.state.snapshot().unwrap().revision;
    host.state
        .update_server(
            vesta_control::machine::owners::Actor::Account(owner),
            &entry,
            revision,
        )
        .unwrap();

    let sources = restore_sources(&host);
    let source = sources
        .iter()
        .find(|s| s["sourceId"] == format!("{}{snapshot}", origin.simple()))
        .expect("the deleted server's backup")
        .clone();
    assert_eq!(source["label"], "Original");
    assert_eq!(source["minecraftVersion"], latest.as_str());
    let job = host.create_plan(restore_body(
        "Restored Real",
        source["sourceId"].as_str().unwrap(),
        free_port(),
    ));
    assert_eq!(job["state"], "review", "{job}");
    // A created server's software lives outside its backup: the restore
    // installs exactly that software and reviews it exactly like `new`.
    let summary = &job["review"]["summary"];
    assert_eq!(summary["restore"]["installSoftware"], true, "{job}");
    assert_eq!(
        summary["software"],
        json!({"kind":"vanilla","version":latest,"build":null,"loaderVersion":null})
    );
    let new = host.create_plan(
        json!({"kind":"new","name":"Compare","minecraftVersion":latest,
        "software":{"kind":"vanilla","build":null,"loaderVersion":null},
        "gamePort":free_port(),"memoryMiB":1024,"eulaAccepted":false}),
    );
    assert_eq!(job["review"]["warnings"], new["review"]["warnings"]);
    assert_eq!(
        host.job_action(new["jobId"].as_str().unwrap(), "cancel").0,
        200
    );
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "real-restore", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let restored = accepted["serverId"].as_str().unwrap().to_owned();
    let ready = host.create_wait(&id, Duration::from_secs(900), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    let restored_dir = host.temp.path().join("servers").join(&restored);
    assert_eq!(
        std::fs::read(restored_dir.join("game/world/vesta-restore-proof.txt")).unwrap(),
        b"backed-up world",
        "the backed-up world"
    );
    assert!(restored_dir.join("install").is_dir());
    assert_eq!(host.job_action(&id, "start").0, 202);
    let online = host.create_wait(&id, Duration::from_secs(600), |j| j["state"] != "starting");
    if online["state"] != "online" {
        let dir = host.temp.path().join("servers").join(&restored);
        eprintln!(
            "launch.toml:\n{}\nlatest.log:\n{}",
            std::fs::read_to_string(dir.join("private/launch.toml")).unwrap_or_default(),
            std::fs::read_to_string(dir.join("game/logs/latest.log")).unwrap_or_default()
        );
    }
    assert_eq!(online["state"], "online", "{online}");
    let card = host
        .servers()
        .into_iter()
        .find(|s| s["serverId"] == restored.as_str())
        .unwrap();
    assert_eq!(card["software"]["kind"], "vanilla");
    stop_server(&host, &restored, "real-restore-stop-0001");
}

/// A real loader-less Modrinth pack ("Miami Beach", a server-only map pack
/// for Vanilla 1.21.11, about 94 MB): created on Vanilla with its files, one
/// software install, then online.
#[test]
#[ignore = "downloads Java, Vanilla and a real Modrinth map pack, then runs the server"]
fn real_loader_less_modrinth_pack_creates_on_vanilla_and_reaches_online() {
    let scratch = tempfile::tempdir().unwrap();
    let java_cache = scratch.path().join("java");
    std::fs::create_dir_all(&java_cache).unwrap();
    let env = Arc::new(WindowsSafeEnv(HostEnvironment {
        java_cache: java_cache.clone(),
    }));
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("servers")).unwrap();
    let servers = std::fs::canonicalize(temp.path().join("servers")).unwrap();
    let installer = Arc::new(vesta_control::machine::creates::LiveCreateInstaller::new(
        java_cache, servers,
    ));
    let live =
        Arc::new(vesta_control::machine::creates::LiveCatalogue::open(scratch.path()).unwrap());
    let host = Host::start(temp, env, Some(installer), None, Some(live));
    let job = host.create_plan(json!({"kind":"modpack","name":"Beach Real",
        "source":{"kind":"provider","provider":"modrinth","projectId":"IswAxbxA","versionId":"cI7lkHeh"},
        "gamePort":free_port(),"memoryMiB":2048,"eulaAccepted":false}));
    assert_eq!(job["state"], "review", "{job}");
    let summary = &job["review"]["summary"];
    assert_eq!(summary["software"]["kind"], "vanilla");
    assert_eq!(summary["pack"]["loader"], "vanilla");
    assert_eq!(summary["minecraftVersion"], "1.21.11");
    let id = job["jobId"].as_str().unwrap().to_owned();
    let (status, accepted) = host.create_apply(&job, "real-vanilla-pack", json!({}));
    assert_eq!(status, 202, "{accepted}");
    let server = accepted["serverId"].as_str().unwrap().to_owned();
    let ready = host.create_wait(&id, Duration::from_secs(1800), |j| {
        !matches!(j["state"].as_str(), Some("applying" | "starting"))
            && (j["state"] != "ready" || !j["error"].is_null())
    });
    assert_eq!(ready["state"], "online", "{ready}");
    host.router
        .import_worker(server.parse().unwrap())
        .expect("worker");
    let software = software_status(&host, &server);
    assert_eq!(software["revision"], 0, "{software}");
    assert_eq!(software["current"]["software"], "vanilla", "{software}");
    assert_eq!(host.job_action(&id, "start").0, 202);
    let online = host.create_wait(&id, Duration::from_secs(600), |j| j["state"] != "starting");
    if online["state"] != "online" {
        let dir = host.temp.path().join("servers").join(&server);
        eprintln!(
            "latest.log:\n{}",
            std::fs::read_to_string(dir.join("game/logs/latest.log")).unwrap_or_default()
        );
    }
    assert_eq!(online["state"], "online", "{online}");
    stop_server(&host, &server, "real-vanilla-pack-stop-0001");
}

#[test]
fn owners_remove_running_and_stopped_servers_and_replay() {
    for stop_first in [false,true] {
        let host=create_host(Arc::new(FakeInstaller::default()));
        let job=host.create_plan(new_body("Remove me",Some(free_port()),Some(1024)));
        let id=job["jobId"].as_str().unwrap();
        let (status,accepted)=host.create_apply(&job,"remove-create",json!({}));assert_eq!(status,202,"{accepted}");
        let server=accepted["serverId"].as_str().unwrap();
        let ready=host.create_wait(id,Duration::from_secs(120),|j| j["state"]=="online" || j["state"]=="failed");assert_eq!(ready["state"],"online","{ready}");
        if stop_first {
            let worker=host.router.owned_handle(server.parse().unwrap()).unwrap();
            let device=host.state.devices(Actor::LocalRecovery).unwrap().into_iter().find(|d| d.device_id==host.devices.0).unwrap();
            let subject=subject(&host.router,&worker,&device).ok().expect("owner subject");let network=worker.authorize(subject).unwrap();
            let generation=network.runtime().game_snapshot().unwrap().generation.unwrap();
            let actor=vesta_control::identity::Subject {principal:vesta_control::identity::PrincipalId::parse(&device.account_id.to_string()).unwrap(),device:vesta_control::identity::DeviceId::parse(&device.device_id.to_string()).unwrap()};
            network.runtime().submit_game(&actor,vesta_control::operations::GameCommand::Stop {expected_generation:generation},"remove-pre-stop").unwrap();
            let until=std::time::Instant::now()+Duration::from_secs(30);
            while network.runtime().game_snapshot().unwrap().phase!=vesta_control::operations::ProcessPhase::Stopped {assert!(std::time::Instant::now()<until);std::thread::sleep(Duration::from_millis(50));}
        }
        let path=format!("/v1/host/servers/{server}");
        let body=json!({"confirmName":"Remove me","idempotencyKey":Uuid::new_v4()});
        let (status,missing)=host.call("DELETE",&format!("/v1/host/servers/{}",Uuid::new_v4()),body.clone(),&host.owner);
        assert_eq!(status,404,"{missing}");assert_eq!(missing["error"]["code"],"not_found");
        host.state.write(|c| {c.execute("UPDATE machine_import_jobs SET state='applying' WHERE server_id=?",[server])?;Ok(())}).unwrap();
        let (status,busy)=host.call("DELETE",&path,body.clone(),&host.owner);assert_eq!(status,409,"{busy}");assert_eq!(busy["error"]["code"],"server_busy");
        host.state.write(|c| {c.execute("UPDATE machine_import_jobs SET state='online' WHERE server_id=?",[server])?;Ok(())}).unwrap();
        let stranger=host.state.devices(Actor::LocalRecovery).unwrap().into_iter().find(|d| d.device_id==host.devices.1).unwrap();
        let owner=host.state.snapshot().unwrap().primary_account_id.unwrap();
        for role in [None,Some(ServerRole::Admin),Some(ServerRole::Moderator),Some(ServerRole::Player)] {
            host.state.set_server_role(Actor::Account(owner),server.parse().unwrap(),stranger.account_id,role,host.state.snapshot().unwrap().revision).unwrap();
            let (status,reply)=host.call("DELETE",&path,body.clone(),&host.stranger);assert_eq!(status,403,"{reply}");
        }
        let (status,wrong)=host.call("DELETE",&path,json!({"confirmName":"wrong","idempotencyKey":Uuid::new_v4()}),&host.owner);assert_eq!(status,422,"{wrong}");
        let (status,removed)=host.call("DELETE",&path,body.clone(),&host.owner);assert_eq!(status,200,"{removed}");
        assert!(host.servers().is_empty());
        let (status,replay)=host.call("DELETE",&path,body,&host.owner);assert_eq!(status,200,"{replay}");assert_eq!(removed,replay);
        assert_eq!(host.call("GET",&format!("/v1/servers/{server}/manage/users"),Value::Null,&host.owner).0,404);
        vesta_control::machine::removed::restore(&host.state,&host.temp.path().join("servers"),server.parse().unwrap()).unwrap();
        assert_eq!(host.servers().len(),1);
        assert_eq!(host.call("GET",&format!("/v1/servers/{server}/manage/server/status"),Value::Null,&host.owner).0,200);
    }
}

#[path = "removal_tests.rs"]
mod removal_tests;

#[path = "storage_tests.rs"]
mod storage_tests;
