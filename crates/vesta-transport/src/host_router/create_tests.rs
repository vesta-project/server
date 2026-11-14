//! `new` server creation through the host router with a fake installer seam
//! (no downloads; the "game" is this test binary acting as an inert fixture),
//! and one ignored real end-to-end creation of Vanilla plus Fabric.
use super::*;
use std::sync::atomic::AtomicBool;
use vesta_control::machine::creates::{CreateInstaller, CreateProgress, InstallRequest};
use vesta_server_install::{
    JavaRequirement, ManagedLaunch, MojangObject, ResolvedPreview, Selection, Software, Warning,
};

const FIXTURE_MARKER: &str = "vesta-create-fixture";
const FIXTURE_ENABLE: &[u8] = b"vesta-owned-inert-create-fixture-v1";
const FIXTURE_TEST: &str = "host_router::tests::imports::creates::inert_game_fixture";

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
}
impl CreateInstaller for FakeInstaller {
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
    fn plan_java(&self, required_major: u32) -> vesta_control::java::HostJavaPlan {
        vesta_control::java::plan_for_major(&[], Some(required_major))
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
        let exe = software.join(if cfg!(windows) {
            "fixture.exe"
        } else {
            "fixture"
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

impl Host {
    fn create_wait(&self, job: &str, timeout: Duration, done: impl Fn(&Value) -> bool) -> Value {
        let until = std::time::Instant::now() + timeout;
        loop {
            let (status, value) = self.call(
                "GET",
                &format!("/v1/host/creates/jobs/{job}"),
                Value::Null,
                &self.owner,
            );
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
            std::thread::sleep(Duration::from_millis(
                if timeout > Duration::from_secs(60) {
                    1000
                } else {
                    100
                },
            ));
        }
    }
    fn create_plan(&self, body: Value) -> Value {
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
        Arc::new(FakeEnv::default()),
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
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| j["state"] != "applying");
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["phase"], "registering");
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
    assert_eq!(servers[0]["state"], "registered");
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
    assert_eq!(started["state"], "starting", "{started}");
    assert_eq!(started["phase"], "starting");
    let online = host.create_wait(&id, Duration::from_secs(120), |j| j["state"] != "starting");
    assert_eq!(online["state"], "online", "{online}");
    assert_eq!(host.servers()[0]["state"], "online");

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
    // Modpack and restore are the next packages: 501 for owners.
    for body in [
        json!({"kind":"modpack","name":"Adventure","source":{"kind":"provider","provider":"modrinth","projectId":"p","versionId":"v"},"gamePort":25566,"memoryMiB":4096,"eulaAccepted":false}),
        json!({"kind":"restore","name":"Back","source":{"kind":"snapshot","sourceId":"s"},"gamePort":25566,"memoryMiB":4096,"eulaAccepted":false}),
    ] {
        let (status, value) = host.call("POST", "/v1/host/creates/plan", body, &host.owner);
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (501, json!("not_supported_yet"))
        );
    }
    for (method, path) in [
        ("GET", "/v1/host/creates/restore-sources"),
        ("POST", "/v1/host/creates/uploads"),
    ] {
        let (status, value) = host.call(method, path, Value::Null, &host.owner);
        assert_eq!(
            (status, value["error"]["code"].clone()),
            (501, json!("not_supported_yet")),
            "{path}"
        );
    }
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
    let done = host.create_wait(&id, Duration::from_secs(30), |j| j["state"] != "applying");
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
        let ready = host.create_wait(&id, Duration::from_secs(60), |j| j["state"] != "applying");
        assert_eq!(ready["state"], "ready", "{ready}");
        created.push((id, defaults));
    }
    let (first, second) = (&created[0].1, &created[1].1);
    assert_eq!(first["name"], "Minecraft Server");
    assert_eq!(second["name"], "Minecraft Server 2");
    assert_eq!(
        (first["gamePort"].clone(), second["gamePort"].clone()),
        (json!(25565), json!(25566))
    );
    // 16 GiB host: 12 GiB budget, both suggestions fit inside it together.
    let total = first["memoryMiB"].as_u64().unwrap() + second["memoryMiB"].as_u64().unwrap();
    assert!(total <= 12 * 1024, "{total}");
    let servers = host.servers();
    assert_eq!(servers.len(), 2);
    assert_ne!(servers[0]["gamePort"], servers[1]["gamePort"]);
    // The budget is spent: another server is refused at review.
    let third = host.create_plan(new_body("Third", None, Some(1024)));
    assert_eq!(
        third["review"]["blockers"],
        json!(["memory_budget_exceeded"])
    );

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
    let ready = host.create_wait(&id, Duration::from_secs(60), |j| j["state"] != "applying");
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(installer.installs.load(Ordering::SeqCst), 1);
    let servers = host.servers();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["serverId"], accepted["serverId"]);
    assert_eq!(servers[0]["gamePort"], 25590);
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
        let ready = host.create_wait(&id, Duration::from_secs(900), |j| j["state"] != "applying");
        assert_eq!(ready["state"], "ready", "{ready}");
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
