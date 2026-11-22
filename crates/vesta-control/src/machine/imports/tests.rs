use super::{
    amp::{self, CandidateScan},
    browse::*,
    inspection::{self, InspectRequest, Loader},
    *,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

fn owner() -> OwnerBinding {
    OwnerBinding {
        host_id: "host".into(),
        account_id: "account".into(),
        device_id: "device".into(),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn prepared_picker_root_exposes_only_published_folders_inside_host_storage() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("host");
    let root = data.join(prepare::PREPARED_DIRECTORY);
    let id = uuid::Uuid::new_v4().to_string();
    let wrapper = root.join(&id);
    let source = wrapper.join("world-server");
    let pending = root.join(format!(".{id}.pending"));
    let servers = data.join("servers");
    for path in [&source, &pending, &servers] {
        fs::create_dir_all(path).unwrap();
    }
    for path in [&wrapper, &source, &pending] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let browser = FolderBrowser::new(vec![], vec![data.clone(), servers.clone()])
        .unwrap()
        .with_test_prepared_root(root.clone());
    let roots = browser.roots().unwrap();
    let prepared = roots
        .roots
        .iter()
        .find(|entry| entry.display_path == root.to_str().unwrap())
        .unwrap();
    let token = browser
        .resolve(
            &owner(),
            &ResolveRequest {
                root_id: prepared.root_id.clone(),
                relative_path: String::new(),
            },
        )
        .unwrap();
    let listing = browser
        .list(
            &owner(),
            &ListRequest {
                folder_token: token.folder_token,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        [id.as_str()]
    );
    let checked = browser.check(&owner(), source.to_str().unwrap()).unwrap();
    assert_eq!(checked.status, "ok");
    let token = checked.folder_token.unwrap();
    assert_eq!(browser.open(&owner(), &token).unwrap().path, source);
    for path in [&data, &servers, &pending] {
        assert_eq!(
            browser
                .check(&owner(), path.to_str().unwrap())
                .unwrap()
                .status,
            "protected"
        );
    }
    fs::rename(&source, servers.join("game")).unwrap();
    assert!(browser.open(&owner(), &token).is_err());
}
fn root_config(path: &Path) -> ImportRootConfig {
    ImportRootConfig {
        root_id: "r1".into(),
        label: "Imports".into(),
        path: path.to_path_buf(),
    }
}
fn browser(path: &Path) -> FolderBrowser {
    FolderBrowser::new(vec![root_config(path)], Vec::new()).unwrap()
}
fn write(path: PathBuf, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn token(b: &FolderBrowser, relative: &str) -> String {
    b.resolve(
        &owner(),
        &ResolveRequest {
            root_id: "r1".into(),
            relative_path: relative.into(),
        },
    )
    .unwrap()
    .folder_token
}
fn inspect(b: &FolderBrowser, relative: &str) -> inspection::InspectionReport {
    inspection::inspect(
        b,
        &owner(),
        &InspectRequest {
            folder_token: token(b, relative),
            ..InspectRequest::default()
        },
    )
    .unwrap()
}
fn world(dir: &Path) {
    write(dir.join("world/level.dat"), "nbt");
}

#[test]
fn detects_vanilla_paper_fabric_forge_and_neoforge_layouts() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let vanilla = base.join("vanilla");
    write(
        vanilla.join("server.properties"),
        "server-port=25601\nenable-rcon=true\nrcon.password=hunter2\n",
    );
    write(vanilla.join("minecraft_server.1.12.2.jar"), "");
    world(&vanilla);
    let paper = base.join("paper");
    write(paper.join("server.properties"), "");
    write(paper.join("paper-1.21.1-123.jar"), "");
    write(paper.join("bukkit.yml"), "");
    write(paper.join("cache/mojang_1.21.1.jar"), "");
    fs::create_dir_all(paper.join("plugins")).unwrap();
    // AMP: Paper as paperclip.jar beside the vanilla jar it patches.
    let amp = base.join("amp-paper");
    write(amp.join("server.properties"), "");
    write(amp.join("paperclip.jar"), "");
    write(amp.join("minecraft_server.jar"), "");
    write(amp.join("bukkit.yml"), "");
    write(amp.join("config/paper-global.yml"), "");
    write(amp.join("version_history.json"), "{\"currentVersion\":\"git-Paper-1 (MC: 1.21.1)\"}");
    fs::create_dir_all(amp.join("plugins")).unwrap();
    let fabric = base.join("fabric");
    write(fabric.join("server.properties"), "");
    write(fabric.join("fabric-server-launch.jar"), "");
    write(fabric.join("server.jar"), "");
    fs::create_dir_all(fabric.join(".fabric")).unwrap();
    fs::create_dir_all(fabric.join("versions/1.21.1")).unwrap();
    let argfile = if cfg!(windows) {
        "win_args.txt"
    } else {
        "unix_args.txt"
    };
    let forge = base.join("forge");
    write(forge.join("server.properties"), "");
    write(forge.join("user_jvm_args.txt"), "");
    write(
        forge.join(format!(
            "libraries/net/minecraftforge/forge/1.20.1-47.3.0/{argfile}"
        )),
        "",
    );
    let neo = base.join("neo");
    write(neo.join("server.properties"), "");
    write(
        neo.join(format!(
            "libraries/net/neoforged/neoforge/21.1.77/{argfile}"
        )),
        "",
    );
    let legacy = base.join("legacy-forge");
    write(legacy.join("server.properties"), "");
    write(legacy.join("forge-1.12.2-14.23.5.2859.jar"), "");
    write(legacy.join("forge-1.12.2-14.23.5.2859-installer.jar"), "");

    let b = browser(base);
    let report = inspect(&b, "vanilla");
    assert!(report.minecraft);
    assert_eq!(report.detected.loader, Some(Loader::Vanilla));
    assert_eq!(report.detected.minecraft_version.as_deref(), Some("1.12.2"));
    assert_eq!(
        report.detected.launch.as_ref().unwrap().relative_path,
        "minecraft_server.1.12.2.jar"
    );
    assert_eq!(report.server.game_port, Some(25601));
    assert_eq!(report.server.rcon_port, Some(25575));
    assert!(report.evidence.contains(&"world_directory"));
    assert!(!serde_json::to_string(&report).unwrap().contains("hunter2"));
    assert_eq!(report.size.files, 3);
    assert!(report.size.complete);

    let report = inspect(&b, "paper");
    assert_eq!(report.detected.loader, Some(Loader::Paper));
    assert_eq!(report.detected.minecraft_version.as_deref(), Some("1.21.1"));
    assert_eq!(
        report.detected.launch.unwrap().relative_path,
        "paper-1.21.1-123.jar"
    );

    let report = inspect(&b, "amp-paper");
    assert_eq!(report.detected.loader, Some(Loader::Paper));
    assert_eq!(report.detected.launch.unwrap().relative_path, "paperclip.jar");
    assert_eq!(report.detected.launch_candidates, ["minecraft_server.jar", "paperclip.jar"]);

    let report = inspect(&b, "fabric");
    assert_eq!(report.detected.loader, Some(Loader::Fabric));
    assert_eq!(report.detected.minecraft_version.as_deref(), Some("1.21.1"));
    assert_eq!(
        report.detected.launch.unwrap().relative_path,
        "fabric-server-launch.jar"
    );

    let report = inspect(&b, "forge");
    assert_eq!(report.detected.loader, Some(Loader::Forge));
    assert_eq!(report.detected.minecraft_version.as_deref(), Some("1.20.1"));
    let launch = report.detected.launch.unwrap();
    assert_eq!(launch.kind, "java_argfile");
    assert_eq!(
        launch.relative_path,
        format!("libraries/net/minecraftforge/forge/1.20.1-47.3.0/{argfile}")
    );

    let report = inspect(&b, "neo");
    assert_eq!(report.detected.loader, Some(Loader::NeoForge));
    assert_eq!(report.detected.minecraft_version.as_deref(), Some("1.21.1"));

    let report = inspect(&b, "legacy-forge");
    assert_eq!(report.detected.loader, Some(Loader::Forge));
    assert_eq!(report.detected.minecraft_version.as_deref(), Some("1.12.2"));
    assert_eq!(
        report.detected.launch.unwrap().relative_path,
        "forge-1.12.2-14.23.5.2859.jar"
    );
}

#[test]
fn unknown_software_and_non_minecraft_folders_are_blocked_not_guessed() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path().join("plain/notes.txt"), "");
    write(temp.path().join("odd/server.properties"), "");
    write(temp.path().join("odd/a.jar"), "");
    write(temp.path().join("odd/b.jar"), "");
    let b = browser(temp.path());
    let report = inspect(&b, "plain");
    assert!(!report.minecraft);
    assert!(report.blockers.contains(&"not_minecraft_server"));
    assert!(report.found.is_empty());
    assert_eq!(
        report.missing,
        vec!["server_properties", "server_jar", "world"]
    );
    let report = inspect(&b, "odd");
    assert!(report.minecraft);
    assert_eq!(report.detected.loader, None);
    assert!(report.blockers.contains(&"software_unresolved"));
    assert!(report.blockers.contains(&"launch_unresolved"));
    assert!(report.found.contains(&"server_properties"));
    assert!(report.missing.contains(&"world"));
}

#[test]
fn amp_tree_yields_candidates_without_reading_amp_config() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("amp");
    let instance = home.join(".ampdata/instances/Survival01");
    write(
        instance.join("AMPConfig.conf"),
        "Login.Password=topsecret\n",
    );
    write(
        instance.join("Minecraft/server.properties"),
        "server-port=25570\n",
    );
    write(instance.join("Minecraft/paper-1.21.1-1.jar"), "");
    world(&instance.join("Minecraft"));
    write(home.join("other/readme.txt"), "");
    let b = browser(&home);
    let scan = CandidateScan {
        amp_datastores: vec![home.join(".ampdata/instances")],
        common_locations: Vec::new(),
        service_user: "vesta".into(),
    };
    let result = amp::candidates(&b, &owner(), &scan).unwrap();
    assert_eq!(result.candidates.len(), 1, "{result:?}");
    let candidate = &result.candidates[0];
    assert_eq!(candidate.kind, "amp");
    assert_eq!(candidate.label, "Minecraft");
    assert_eq!(candidate.confidence, "high");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("topsecret")
    );
    let report = inspection::inspect(
        &b,
        &owner(),
        &InspectRequest {
            folder_token: candidate.folder_token.clone().expect("readable candidate"),
            ..InspectRequest::default()
        },
    )
    .unwrap();
    assert_eq!(report.detected.kind, "amp");
    assert_eq!(report.detected.instance_name.as_deref(), Some("Survival01"));
    assert!(report.warnings.contains(&"disable_amp_auto_restart"));
}

#[test]
fn manager_instances_outside_saved_roots_are_still_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let datastore = temp.path().join("ampdata/instances");
    write(datastore.join("Creative/Minecraft/server.properties"), "");
    let imports = temp.path().join("imports");
    fs::create_dir_all(&imports).unwrap();
    let b = browser(&imports);
    let scan = CandidateScan {
        amp_datastores: vec![datastore],
        common_locations: Vec::new(),
        service_user: "vesta".into(),
    };
    let result = amp::candidates(&b, &owner(), &scan).unwrap();
    assert_eq!(result.candidates.len(), 1, "{result:?}");
    let candidate = &result.candidates[0];
    assert_eq!(candidate.kind, "amp");
    assert_eq!(candidate.source, "server_manager");
    assert!(candidate.readable);
    assert!(candidate.folder_token.is_some());
    assert!(result.warnings.is_empty());
}

#[test]
fn instances_json_keeps_only_minecraft_names_and_paths() {
    let custom = if cfg!(windows) {
        r"D:\\amp\\Custom"
    } else {
        "/srv/amp/Custom"
    };
    let json = format!(
        r#"[{{"InstanceName":"Survival","Module":"Minecraft","AdminPassword":"x"}},
        {{"InstanceName":"Web","Module":"ADS"}},
        {{"InstanceName":"../escape","Module":"Minecraft"}},
        {{"InstanceName":"Custom","Module":"MinecraftModule","DataPath":"{custom}"}}]"#
    );
    let datastore = Path::new("/home/amp/.ampdata/instances");
    let found = amp::parse_instances(json.as_bytes(), datastore);
    let custom = if cfg!(windows) {
        r"D:\amp\Custom"
    } else {
        "/srv/amp/Custom"
    };
    assert_eq!(
        found,
        vec![
            datastore.join("Survival").join("Minecraft"),
            PathBuf::from(custom).join("Minecraft"),
        ]
    );
}

#[test]
fn remediation_is_narrow_and_never_changes_ownership() {
    let path = Path::new("/home/amp/.ampdata/instances/Survival/Minecraft");
    let fix = amp::remediation(path, "vesta; rm -rf /");
    assert!(!fix.command.contains("chown") && !fix.command.contains(';'));
    if !cfg!(windows) {
        assert_eq!(
            fix.command,
            "sudo vesta import-root add '/home/amp/.ampdata/instances/Survival/Minecraft'"
        );
        assert!(!fix.command.contains("setfacl"));
    }
}

#[test]
fn relative_paths_refuse_traversal_absolute_and_separators() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("a/b")).unwrap();
    let b = browser(temp.path());
    for bad in [
        "../x", "/a", "a//b", "a\\b", "a/./b", "a/..", "C:/x", "a\0b",
    ] {
        let error = b
            .resolve(
                &owner(),
                &ResolveRequest {
                    root_id: "r1".into(),
                    relative_path: bad.into(),
                },
            )
            .unwrap_err();
        assert_eq!(error, ImportError::InvalidPath, "{bad}");
    }
    let unknown = b.resolve(
        &owner(),
        &ResolveRequest {
            root_id: "other".into(),
            relative_path: "a".into(),
        },
    );
    assert_eq!(unknown.unwrap_err(), ImportError::UnknownRoot);
    assert!(
        b.resolve(
            &owner(),
            &ResolveRequest {
                root_id: "r1".into(),
                relative_path: "a/b".into(),
            }
        )
        .is_ok()
    );
}

#[test]
fn tokens_are_owner_bound_expire_and_detect_swaps() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("a")).unwrap();
    let clock = Arc::new(AtomicU64::new(1_000));
    let reader = clock.clone();
    let b = FolderBrowser::with_clock(
        vec![root_config(temp.path())],
        Vec::new(),
        Arc::new(move || reader.load(Ordering::SeqCst)),
    )
    .unwrap();
    let t = token(&b, "a");
    let mut other = owner();
    other.device_id = "other-device".into();
    let list = |binding: &OwnerBinding, token: &str| {
        b.list(
            binding,
            &ListRequest {
                folder_token: token.into(),
                cursor: None,
            },
        )
    };
    assert_eq!(list(&other, &t).unwrap_err(), ImportError::TokenInvalid);
    assert!(list(&owner(), &t).is_ok());
    // Swap the directory for a different one at the same name.
    fs::rename(temp.path().join("a"), temp.path().join("old")).unwrap();
    fs::create_dir_all(temp.path().join("a")).unwrap();
    assert_eq!(list(&owner(), &t).unwrap_err(), ImportError::Changed);
    let fresh = token(&b, "a");
    clock.store(1_000 + TOKEN_TTL_MS, Ordering::SeqCst);
    assert_eq!(
        list(&owner(), &fresh).unwrap_err(),
        ImportError::TokenExpired
    );
    assert_eq!(
        list(&owner(), "not-a-token").unwrap_err(),
        ImportError::TokenInvalid
    );
    // Removing the root from configuration invalidates its tokens.
    clock.store(1_000, Ordering::SeqCst);
    let again = token(&b, "a");
    b.replace_roots(Vec::new()).unwrap();
    assert_eq!(
        list(&owner(), &again).unwrap_err(),
        ImportError::UnknownRoot
    );
}

#[test]
fn listing_is_directories_only_and_paged() {
    let temp = tempfile::tempdir().unwrap();
    for i in 0..150 {
        fs::create_dir_all(temp.path().join(format!("d{i:03}"))).unwrap();
    }
    write(temp.path().join("file.txt"), "contents");
    let b = browser(temp.path());
    let t = token(&b, "");
    let first = b
        .list(
            &owner(),
            &ListRequest {
                folder_token: t.clone(),
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(first.entries.len(), PAGE_SIZE);
    assert!(
        first
            .entries
            .iter()
            .all(|e| e.kind == "directory" && e.readable)
    );
    assert!(first.entries.iter().all(|e| e.folder_token.is_some()));
    let second = b
        .list(
            &owner(),
            &ListRequest {
                folder_token: t,
                cursor: first.next_cursor.clone(),
            },
        )
        .unwrap();
    assert_eq!(second.entries.len(), 50);
    assert_eq!(second.entries[0].name, "d100");
    assert!(second.next_cursor.is_none());
}

#[test]
fn roots_overlapping_protected_state_or_filesystem_root_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let inside = FolderBrowser::new(vec![root_config(temp.path())], vec![state.clone()]);
    assert_eq!(inside.err(), Some(ImportError::ProtectedPath));
    let below = FolderBrowser::new(vec![root_config(&state)], vec![temp.path().to_path_buf()]);
    assert_eq!(below.err(), Some(ImportError::ProtectedPath));
    let filesystem_root = if cfg!(windows) {
        PathBuf::from("C:\\")
    } else {
        PathBuf::from("/")
    };
    assert_eq!(
        FolderBrowser::new(vec![root_config(&filesystem_root)], Vec::new()).err(),
        Some(ImportError::InvalidRoot)
    );
    assert_eq!(
        FolderBrowser::new(vec![root_config(Path::new("relative"))], Vec::new()).err(),
        Some(ImportError::InvalidRoot)
    );
}

fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link).is_ok();
    #[cfg(windows)]
    return std::os::windows::fs::symlink_dir(target, link).is_ok();
}

#[test]
fn symlink_escapes_are_refused_and_not_listed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    write(outside.join("server.properties"), "");
    fs::create_dir_all(&root).unwrap();
    if !link_dir(&outside, &root.join("escape")) {
        eprintln!("symlink creation not permitted; skipping");
        return;
    }
    let b = browser(&root);
    let error = b
        .resolve(
            &owner(),
            &ResolveRequest {
                root_id: "r1".into(),
                relative_path: "escape".into(),
            },
        )
        .unwrap_err();
    assert_eq!(error, ImportError::LinkedPath);
    let listing = b
        .list(
            &owner(),
            &ListRequest {
                folder_token: token(&b, ""),
                cursor: None,
            },
        )
        .unwrap();
    assert!(listing.entries.is_empty());
    let scan = CandidateScan {
        amp_datastores: Vec::new(),
        common_locations: Vec::new(),
        service_user: "vesta".into(),
    };
    assert!(
        amp::candidates(&b, &owner(), &scan)
            .unwrap()
            .candidates
            .is_empty()
    );
    // A linked root is refused outright.
    let linked_root = temp.path().join("linked-root");
    if link_dir(&outside, &linked_root) {
        assert_eq!(
            FolderBrowser::new(vec![root_config(&linked_root)], Vec::new()).err(),
            Some(ImportError::LinkedPath)
        );
    }
    // A link inside a source folder blocks inspection as unsafe.
    write(root.join("server/server.properties"), "");
    if link_dir(&outside, &root.join("server/world")) {
        assert!(inspect(&b, "server").blockers.contains(&"unsafe_path"));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_directories_report_source_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!("running as root; permissions are not enforced");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let locked = temp.path().join("locked");
    write(locked.join("server.properties"), "");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let b = browser(temp.path());
    let t = token(&b, "locked");
    let result = b.list(
        &owner(),
        &ListRequest {
            folder_token: t.clone(),
            cursor: None,
        },
    );
    let report = inspection::inspect(
        &b,
        &owner(),
        &InspectRequest {
            folder_token: t,
            ..InspectRequest::default()
        },
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(result.unwrap_err(), ImportError::Unreadable);
    assert_eq!(report.unwrap().blockers, vec!["source_unreadable"]);
}
#[cfg(unix)]
fn running_as_root() -> bool {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .map(|l| l.split_whitespace().nth(1) == Some("0"))
        })
        .unwrap_or(false)
}

#[test]
fn occupied_game_port_blocks_import() {
    let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let temp = tempfile::tempdir().unwrap();
    write(
        temp.path().join("s/server.properties"),
        &format!("server-port={port}\n"),
    );
    write(temp.path().join("s/server.jar"), "");
    let b = browser(temp.path());
    let report = inspect(&b, "s");
    let evidence = report.ports.iter().find(|p| p.port == port).unwrap();
    assert_eq!(evidence.state, "occupied");
    assert!(report.blockers.contains(&"game_port_in_use"));
    drop(listener);
    let report = inspect(&b, "s");
    assert!(!report.blockers.contains(&"game_port_in_use"));
}

#[cfg(target_os = "linux")]
#[test]
fn running_process_in_source_blocks_import() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("s");
    write(source.join("server.properties"), "");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(&source)
        .spawn()
        .unwrap();
    let b = browser(temp.path());
    let report = inspect(&b, "s");
    let _ = child.kill();
    let _ = child.wait();
    let process = report
        .processes
        .iter()
        .find(|p| p.pid == child.id())
        .expect("source process reported");
    assert_eq!(process.relation, "source");
    assert!(process.start_identity.is_some());
    assert!(report.blockers.contains(&"source_running"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unobservable_processes_are_unknown_not_clear() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path().join("s/server.properties"), "");
    let b = browser(temp.path());
    let report = inspect(&b, "s");
    assert_eq!(report.inspection.processes, "unknown");
    assert!(report.warnings.contains(&"source_process_unknown"));
    assert!(!report.blockers.contains(&"source_running"));
}

#[test]
fn destination_space_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path().join("s/server.properties"), "");
    let b = browser(temp.path());
    let report = inspection::inspect(
        &b,
        &owner(),
        &InspectRequest {
            folder_token: token(&b, "s"),
            destination: Some(temp.path().to_path_buf()),
            ..InspectRequest::default()
        },
    )
    .unwrap();
    let free = report.destination_free_bytes.unwrap();
    assert_eq!(
        report.blockers.contains(&"insufficient_space"),
        free < inspection::SPACE_MARGIN_BYTES
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires python3 and TCP port 25599"]
fn real_amp_process_and_listener_block_then_clear() {
    use std::{
        net::TcpStream,
        process::{Command, Stdio},
        thread,
        time::Duration,
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let relative = ".ampdata/instances/Test01/Minecraft";
    let source = temp.path().join(relative);
    write(source.join("server.properties"), "server-port=25599\n");
    write(source.join("server.jar"), "");
    world(&source);
    let mut child = ChildGuard(
        Command::new("python3")
            .args(["-m", "http.server", "25599", "--bind", "127.0.0.1"])
            .current_dir(&source)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ready = false;
    for _ in 0..100 {
        if child.0.try_wait().unwrap().is_some() {
            panic!("server exited before listening");
        }
        if TcpStream::connect(("127.0.0.1", 25599)).is_ok() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(ready, "server did not start listening");
    let b = browser(temp.path());
    let report = inspect(&b, relative);
    assert_eq!(report.detected.kind, "amp");
    assert!(report.blockers.contains(&"source_running"), "{report:?}");
    assert!(report.blockers.contains(&"game_port_in_use"), "{report:?}");
    assert!(
        report
            .processes
            .iter()
            .any(|p| p.pid == child.0.id() && p.relation == "source")
    );
    assert!(
        report
            .ports
            .iter()
            .any(|p| p.port == 25599 && p.state == "occupied" && p.owner == Some("source")),
        "{report:?}"
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let report = inspect(&b, relative);
    assert!(!report.blockers.contains(&"source_running"), "{report:?}");
    assert!(!report.blockers.contains(&"game_port_in_use"), "{report:?}");
}

#[test]
fn filesystem_roots_are_listed_and_entries_carry_looks_like_server() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("survival");
    write(server.join("server.properties"), "");
    write(server.join("server.jar"), "");
    fs::create_dir_all(temp.path().join("empty")).unwrap();
    let b = FolderBrowser::new(Vec::new(), Vec::new()).unwrap();
    let roots = b.roots().unwrap();
    assert!(
        roots.roots.iter().any(|r| r.kind == "filesystem"),
        "{roots:?}"
    );
    let (root, parts) = b.filesystem_root_for(temp.path()).expect("temp volume");
    let token = b
        .resolve(
            &owner(),
            &ResolveRequest {
                root_id: root.config.root_id,
                relative_path: parts.join("/"),
            },
        )
        .unwrap()
        .folder_token;
    let listing = b
        .list(
            &owner(),
            &ListRequest {
                folder_token: token,
                cursor: None,
            },
        )
        .unwrap();
    let survival = listing
        .entries
        .iter()
        .find(|e| e.name == "survival")
        .expect("survival");
    assert!(survival.looks_like_server);
    assert!(survival.readable);
    let empty = listing
        .entries
        .iter()
        .find(|e| e.name == "empty")
        .expect("empty");
    assert!(!empty.looks_like_server);
    assert!(empty.readable);
    assert!(empty.remediation_command.is_none());
}

#[test]
fn check_reports_each_status() {
    let temp = tempfile::tempdir().unwrap();
    let ok_dir = temp.path().join("ok");
    fs::create_dir_all(&ok_dir).unwrap();
    write(temp.path().join("file.txt"), "x");
    let protected = temp.path().join("private");
    fs::create_dir_all(&protected).unwrap();
    let b = FolderBrowser::new(Vec::new(), vec![protected.clone()]).unwrap();
    let ok = b.check(&owner(), &ok_dir.to_string_lossy()).unwrap();
    assert_eq!(ok.status, "ok");
    assert!(ok.folder_token.is_some());
    assert!(ok.remediation_command.is_none());
    let missing = b
        .check(&owner(), &temp.path().join("no-such").to_string_lossy())
        .unwrap();
    assert_eq!(missing.status, "not_found");
    let file = b
        .check(&owner(), &temp.path().join("file.txt").to_string_lossy())
        .unwrap();
    assert_eq!(file.status, "not_directory");
    let blocked = b.check(&owner(), &protected.to_string_lossy()).unwrap();
    assert_eq!(blocked.status, "protected");
    assert!(b.check(&owner(), "relative").is_err());
}

#[test]
fn candidates_stay_silent_for_bare_unreadable_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let imports = temp.path().join("imports");
    fs::create_dir_all(&imports).unwrap();
    let b = browser(&imports);
    let absent = temp.path().join("absent-home");
    let empty = temp.path().join("empty-home");
    fs::create_dir_all(&empty).unwrap();
    let scan = CandidateScan {
        amp_datastores: vec![absent.join(".ampdata/instances")],
        common_locations: vec![absent, empty],
        service_user: "vesta".into(),
    };
    let result = amp::candidates(&b, &owner(), &scan).unwrap();
    assert!(result.candidates.is_empty(), "{result:?}");
    assert!(result.warnings.is_empty(), "{result:?}");
}

#[cfg(unix)]
#[test]
fn listing_marks_unreadable_children_and_check_needs_permission() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!("running as root; permissions are not enforced");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let locked = temp.path().join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let b = FolderBrowser::new(Vec::new(), Vec::new()).unwrap();
    let (root, parts) = b.filesystem_root_for(temp.path()).expect("temp volume");
    let token = b
        .resolve(
            &owner(),
            &ResolveRequest {
                root_id: root.config.root_id,
                relative_path: parts.join("/"),
            },
        )
        .unwrap()
        .folder_token;
    let listing = b
        .list(
            &owner(),
            &ListRequest {
                folder_token: token,
                cursor: None,
            },
        )
        .unwrap();
    let entry = listing
        .entries
        .iter()
        .find(|e| e.name == "locked")
        .expect("locked listed");
    assert!(!entry.readable);
    assert!(!entry.looks_like_server);
    assert!(
        entry
            .remediation_command
            .as_ref()
            .is_some_and(|c| c.contains("import-root add"))
    );
    let checked = b.check(&owner(), &locked.to_string_lossy()).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(checked.status, "permission_needed");
    assert!(
        checked
            .remediation_command
            .as_ref()
            .is_some_and(|c| c.contains("import-root add"))
    );
}

#[cfg(unix)]
#[test]
fn protected_and_system_trees_are_hidden_when_browsing_filesystem() {
    let b = FolderBrowser::new(Vec::new(), vec![PathBuf::from("/var/lib/vesta")]).unwrap();
    let roots = b.roots().unwrap();
    let fs_root = roots
        .roots
        .iter()
        .find(|r| r.kind == "filesystem")
        .expect("filesystem root");
    let listing = b
        .list(
            &owner(),
            &ListRequest {
                folder_token: b
                    .resolve(
                        &owner(),
                        &ResolveRequest {
                            root_id: fs_root.root_id.clone(),
                            relative_path: String::new(),
                        },
                    )
                    .unwrap()
                    .folder_token,
                cursor: None,
            },
        )
        .unwrap();
    for forbidden in ["proc", "sys", "dev", "run", "boot"] {
        assert!(
            listing.entries.iter().all(|e| e.name != forbidden),
            "{forbidden} listed: {listing:?}"
        );
    }
}

#[test]
fn non_server_inspection_only_reports_cause_with_nearby() {
    let temp = tempfile::tempdir().unwrap();
    write(
        temp.path().join("survival/server.properties"),
        "server-port=25565",
    );
    let b = browser(temp.path());
    let report = inspect(&b, "");
    assert_eq!(report.blockers, vec!["not_minecraft_server"]);
    assert!(report.warnings.is_empty());
    assert_eq!(report.nearby.len(), 1);
    assert!(report.nearby[0].path.ends_with("survival"));
}

#[test]
fn root_loader_observes_local_changes_without_restart() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("added");
    fs::create_dir(&child).unwrap();
    let b = browser(temp.path());
    let config = root_config(&child);
    b.set_root_loader(move || Ok(vec![config.clone()]));
    let roots = b.roots().unwrap();
    assert!(
        roots
            .roots
            .iter()
            .any(|r| r.display_path == display(&child))
    );
}

/// A small jar with the given entries.
fn jar(path: PathBuf, entries: &[(&str, &[u8])]) {
    use std::io::Write;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, body) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(body).unwrap();
    }
    zip.finish().unwrap();
}

/// A Vanilla folder with a plain `server.jar` reports the version the jar
/// declares about itself (`version.json`, or a bundler's `versions.list`),
/// read in memory without running or extracting anything.
#[test]
fn vanilla_server_jar_reports_its_embedded_version() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let cases: [(&str, Vec<(&str, &[u8])>, Option<&str>); 5] = [
        (
            "modern",
            vec![
                ("version.json", br#"{"id":"1.21.1","name":"1.21.1","world_version":3955}"#),
                ("META-INF/versions.list", b"abc\t1.21.1\t1.21.1/server-1.21.1.jar\n"),
            ],
            Some("1.21.1"),
        ),
        (
            "bundler",
            vec![("META-INF/versions.list", b"abc\t1.20.4\t1.20.4/server-1.20.4.jar\n")],
            Some("1.20.4"),
        ),
        ("snapshot", vec![("version.json", br#"{"id":"24w14a"}"#)], Some("24w14a")),
        // Unsafe or missing ids say nothing.
        ("hostile", vec![("version.json", br#"{"id":"../../etc"}"#)], None),
        ("bare", vec![("net/minecraft/Main.class", b"x")], None),
    ];
    for (name, entries, _) in &cases {
        let dir = base.join(name);
        write(dir.join("server.properties"), "");
        world(&dir);
        jar(dir.join("server.jar"), entries);
    }
    // Not a zip at all.
    write(base.join("junk/server.properties"), "");
    write(base.join("junk/server.jar"), "not a jar");
    let b = browser(base);
    for (name, _, expected) in &cases {
        let report = inspect(&b, name);
        assert_eq!(report.detected.loader, Some(Loader::Vanilla), "{name}");
        assert_eq!(report.detected.minecraft_version.as_deref(), *expected, "{name}");
        assert_eq!(
            report.blockers.contains(&"software_unresolved"),
            expected.is_none(),
            "{name}"
        );
    }
    assert_eq!(inspect(&b, "junk").detected.minecraft_version, None);
}
