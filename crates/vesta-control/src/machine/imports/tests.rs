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
    let report = inspect(&b, "odd");
    assert!(report.minecraft);
    assert_eq!(report.detected.loader, None);
    assert!(report.blockers.contains(&"software_unresolved"));
    assert!(report.blockers.contains(&"launch_unresolved"));
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
        service_user: "vesta".into(),
    };
    let result = amp::candidates(&b, &owner(), &scan).unwrap();
    assert_eq!(result.candidates.len(), 1, "{result:?}");
    let candidate = &result.candidates[0];
    assert_eq!(candidate.kind, "amp");
    assert_eq!(candidate.label, "Survival01");
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
            folder_token: candidate.folder_token.clone(),
            ..InspectRequest::default()
        },
    )
    .unwrap();
    assert_eq!(report.detected.kind, "amp");
    assert_eq!(report.detected.instance_name.as_deref(), Some("Survival01"));
    assert!(report.warnings.contains(&"disable_amp_auto_restart"));
}

#[test]
fn amp_instances_outside_roots_are_reported_with_a_local_command() {
    let temp = tempfile::tempdir().unwrap();
    let datastore = temp.path().join("ampdata/instances");
    write(datastore.join("Creative/Minecraft/server.properties"), "");
    let imports = temp.path().join("imports");
    fs::create_dir_all(&imports).unwrap();
    let b = browser(&imports);
    let scan = CandidateScan {
        amp_datastores: vec![datastore],
        service_user: "vesta".into(),
    };
    let result = amp::candidates(&b, &owner(), &scan).unwrap();
    assert!(result.candidates.is_empty());
    let warning = &result.warnings[0];
    assert_eq!(warning.code, "outside_import_roots");
    assert!(
        warning
            .remediation
            .as_ref()
            .unwrap()
            .command
            .contains("vesta import-root add")
    );
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
    assert!(report.unwrap().blockers.contains(&"source_unreadable"));
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
    assert!(report.blockers.contains(&"source_state_unknown"));
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
