//! H17: commit and start.
use super::*;

fn registered(f: &Fixture, folder: &str) -> (ImportJob, ApplyAccepted) {
    let (_, accepted) = applied(f, folder);
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "ready", "{:?}", job.error);
    assert_eq!(job.phase, "register");
    (job, accepted)
}

#[test]
fn commit_records_eula_and_port_and_registers_exactly_once() {
    let f = Fixture::new();
    f.vanilla();
    let (job, accepted) = registered(&f, "vanilla");
    let dir = f.jobs.server_dir(accepted.server_id);
    let eula = fs::read_to_string(dir.join("game/eula.txt")).unwrap();
    assert!(eula.contains("eula=true") && eula.contains(&f.binding.account_id));
    // The source keeps its own EULA answer untouched.
    assert_eq!(
        fs::read_to_string(f.source("vanilla").join("eula.txt")).unwrap(),
        "eula=false\n"
    );
    let properties = fs::read_to_string(dir.join("game/server.properties")).unwrap();
    assert!(properties.contains("server-port=25565") && properties.contains("motd=hi"));
    assert!(dir.join("host.toml").is_file() && dir.join("private/launch.toml").is_file());
    let entry = f.state.server(accepted.server_id).unwrap().unwrap();
    assert_eq!(entry.game_port, 25_565);
    assert_eq!(entry.name, "Survival");
    // Retrying the commit (crash before the state update) changes nothing.
    let review = job.review.clone().unwrap();
    let revision = f.state.snapshot().unwrap().revision;
    f.jobs.commit(&accepted.job_id, &review).unwrap();
    assert_eq!(f.state.snapshot().unwrap().revision, revision);
    let account = Uuid::parse_str(&f.binding.account_id).unwrap();
    assert_eq!(f.state.visible_servers(account).unwrap().len(), 1);
    // Port and memory stay reserved for the committed server.
    let held = f.jobs.read(|c| reservations_in(c, None)).unwrap();
    assert!(held.ports.contains(&25_565));
    assert_eq!(held.memory_mib, 2048);
    // A crash mid-commit resumes into the same registration.
    f.jobs
        .set_phase(&accepted.job_id, "interrupted", "register")
        .unwrap();
    f.jobs.resume(&f.binding, &accepted.job_id).unwrap();
    let job = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(job.state, "ready", "{:?}", job.error);
    assert_eq!(f.state.visible_servers(account).unwrap().len(), 1);
}

#[test]
fn launch_spec_uses_only_detected_facts() {
    let game = std::env::temp_dir().join("vesta-launch-spec").join("game");
    let java = Path::new("/opt/vesta/java/bin/java");
    for (kind, relative, expected_first) in [
        ("jar", "minecraft_server.1.21.1.jar", "-jar"),
        ("jar", "paper-1.21.1-123.jar", "-jar"),
        ("jar", "fabric-server-launch.jar", "-jar"),
    ] {
        let entry = crate::files::copied_export::CopyEntry {
            path: relative.into(),
            directory: false,
            size: 3,
            sha256: "ab".repeat(32),
            identity: String::new(),
        };
        let choice = LaunchChoice {
            kind: kind.into(),
            relative_path: relative.into(),
        };
        let (launch, jvm) = commit::trusted_launch(&game, &choice, java, 3072, &entry).unwrap();
        assert_eq!(launch.java, java);
        assert_eq!(launch.working_directory, game);
        assert_eq!(jvm, vec!["-Xmx3072M".to_owned()]);
        assert!(launch.args[0].starts_with(expected_first));
        assert_eq!(launch.args.last().unwrap(), "nogui");
        let mut path = game.clone();
        path.extend(relative.split('/'));
        assert!(
            launch
                .args
                .iter()
                .any(|a| a.ends_with(path.to_str().unwrap()))
        );
        assert_eq!(launch.required_files, vec![path.clone()]);
        assert_eq!(launch.artifacts[0].sha256, entry.sha256);
        // Nothing from user_jvm_args.txt, run.sh or other source files is used.
        assert!(
            !launch
                .args
                .iter()
                .any(|a| a.contains("user_jvm_args") || a.ends_with(".sh"))
        );
    }
    let entry = crate::files::copied_export::CopyEntry {
        path: "run.sh".into(),
        directory: false,
        size: 1,
        sha256: String::new(),
        identity: String::new(),
    };
    let script = LaunchChoice {
        kind: "script".into(),
        relative_path: "run.sh".into(),
    };
    assert!(commit::trusted_launch(&game, &script, java, 1024, &entry).is_err());
    // Plan requests refuse launch choices that are not a jar or Java argfile.
    let f = Fixture::new();
    f.vanilla();
    let mut request = PlanRequest {
        folder_token: f.token("vanilla"),
        name: "S".into(),
        game_port: None,
        memory_mib: None,
        launch_choice: Some(LaunchChoice {
            kind: "jar".into(),
            relative_path: "run.sh".into(),
        }),
        minecraft_version: None,
    };
    assert_eq!(
        f.jobs.plan(&f.binding, request.clone()).unwrap_err(),
        JobError::INVALID
    );
    request.launch_choice = Some(LaunchChoice {
        kind: "jar".into(),
        relative_path: "../escape.jar".into(),
    });
    assert_eq!(
        f.jobs.plan(&f.binding, request).unwrap_err(),
        JobError::INVALID
    );
    let rewritten = commit::rewrite_properties("enable-rcon true\nserver-port 1\nmotd=x", 25570);
    assert!(rewritten.contains("enable-rcon=false\n"));
    assert!(rewritten.contains("server-port=25570\n"));

}

#[test]
fn rcon_enabled_source_is_disabled_with_an_acknowledged_warning() {
    let f = Fixture::new();
    f.vanilla();
    f.put(
        "vanilla/server.properties",
        "server-port=25565\nenable-rcon=true\nrcon.password=hunter2\n",
    );
    let job = f.plan("vanilla");
    let review = job.review.as_ref().unwrap();
    assert!(review.warnings.contains(&"rcon_disabled".to_owned()));
    assert!(!serde_json::to_string(&job).unwrap().contains("hunter2"));
    let accepted = f
        .jobs
        .apply(&f.binding, &Fixture::apply_request(&job), &key(&job))
        .unwrap();
    let done = f.jobs.run_apply(&f.binding, &accepted.job_id).unwrap();
    assert_eq!(done.state, "ready", "{:?}", done.error);
    let text = fs::read_to_string(
        f.jobs
            .server_dir(accepted.server_id)
            .join("game/server.properties"),
    )
    .unwrap();
    assert!(text.contains("enable-rcon=false\n"));
    assert!(!text.contains("enable-rcon=true"));
}

#[test]
fn worker_is_created_and_start_conflicts_keep_the_copy() {
    let f = Fixture::new();
    f.fabric();
    let (_, accepted) = registered(&f, "fabric");
    let worker = f
        .jobs
        .create_worker(accepted.server_id, Arc::new(SystemClock))
        .unwrap();
    let handle = worker.handle();
    // Bind conflict at start: resolvable error, copy and registration retained.
    *f.env.busy_port.lock().unwrap() = Some(25_565);
    let job = f.jobs.start(&f.binding, &accepted.job_id, &handle).unwrap();
    assert_eq!(job.state, "ready");
    assert_eq!(job.error.as_deref(), Some("game_port_in_use"));
    let game = f.jobs.server_dir(accepted.server_id).join("game");
    assert!(game.join("fabric-server-launch.jar").is_file());
    assert!(f.state.server(accepted.server_id).unwrap().is_some());
    *f.env.busy_port.lock().unwrap() = None;
    f.env.running.store(true, Ordering::SeqCst);
    let job = f.jobs.start(&f.binding, &accepted.job_id, &handle).unwrap();
    assert_eq!(job.error.as_deref(), Some("source_running"));
    f.env.running.store(false, Ordering::SeqCst);
    let stranger = OwnerBinding {
        account_id: Uuid::new_v4().to_string(),
        ..f.binding.clone()
    };
    assert_eq!(
        f.jobs
            .start(&stranger, &accepted.job_id, &handle)
            .unwrap_err(),
        JobError::FORBIDDEN
    );
    // Conflicts resolved: the start is admitted through the authorized runtime.
    let mut job = f.jobs.start(&f.binding, &accepted.job_id, &handle).unwrap();
    assert_eq!(job.state, "starting", "{:?}", job.error);
    // The fixture's Java is not runnable: never `online` without readiness.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while job.state == "starting" && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
        job = f
            .jobs
            .readiness(&f.binding, &accepted.job_id, &handle)
            .unwrap();
    }
    assert_ne!(job.state, "online");
    assert!(game.join("world/level.dat").is_file());
    assert!(matches!(
        worker.shutdown(std::time::Duration::from_secs(10)).unwrap(),
        crate::worker::WorkerShutdown::Complete
    ));
}

#[test]
fn launch_paths_avoid_windows_verbatim_spelling_java_cannot_load() {
    let verbatim = Path::new(r"\\?\C:\vesta\servers\id\game");
    let unc = Path::new(r"\\?\UNC\host\share\game");
    if cfg!(windows) {
        assert_eq!(
            commit::plain_drive_path(verbatim),
            PathBuf::from(r"C:\vesta\servers\id\game")
        );
    } else {
        assert_eq!(commit::plain_drive_path(verbatim), verbatim);
    }
    assert_eq!(commit::plain_drive_path(unc), unc);
    assert_eq!(
        commit::plain_drive_path(Path::new("/var/lib/vesta/servers")),
        Path::new("/var/lib/vesta/servers")
    );
}
