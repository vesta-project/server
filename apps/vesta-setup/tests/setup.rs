use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use vesta_setup::{
    engine::{self, Answers, Receipt, Startup},
    host, platform,
    release::{self, Artifact, Manifest, Source},
    ui, visual_states,
};

struct Bundle {
    temp: tempfile::TempDir,
    key: Ed25519KeyPair,
    manifest: Manifest,
}

#[test]
fn local_server_configuration_requires_an_explicit_copied_export_and_preserves_it() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("installation");
    fs::create_dir(&root).unwrap();
    let export = temporary.path().join("copied-export");
    fs::create_dir(&export).unwrap();
    fs::write(export.join("server.jar"), b"inert-source").unwrap();
    let mut input = answers(root.clone());
    input.existing_server = Some(export.clone());
    let receipt = Receipt {
        schema: 1,
        installation_id: "1".repeat(32),
        answers: input,
        release_version: "fixture".into(),
        release_manifest_sha256: "2".repeat(64),
        phase: "installed".into(),
        tls_sha256: "3".repeat(64),
        startup_registered: false,
        external_status: Default::default(),
    };
    engine::write_private(&root, "setup.json", &serde_json::to_vec(&receipt).unwrap()).unwrap();
    let java = std::env::current_exe().unwrap();
    assert_eq!(
        host::configure_server(&root, java.clone(), Some("server.jar".into()), None, false)
            .unwrap_err(),
        "setup_confirm_copied_export_required"
    );
    assert!(!root.join("private/first-server.json").exists());
    host::configure_server(&root, java.clone(), Some("server.jar".into()), None, true).unwrap();
    let before = fs::read(root.join("private/first-server.json")).unwrap();
    assert!(
        host::configure_server(
            &root,
            java.clone(),
            Some("server.jar".into()),
            Some("args.txt".into()),
            true
        )
        .is_err()
    );
    assert!(
        host::configure_server(&root, java.clone(), None, Some("../args.txt".into()), true)
            .is_err()
    );
    assert!(host::configure_server(&root, java, Some("../server.jar".into()), None, true).is_err());
    assert_eq!(
        fs::read(root.join("private/first-server.json")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(export.join("server.jar")).unwrap(),
        b"inert-source"
    );
}

#[test]
fn update_cli_bootstraps_exact_installed_release_and_stages_without_activation() {
    let mut bundle = Bundle::new(false);
    let cli = if cfg!(windows) { "vesta.exe" } else { "vesta" };
    let bytes = b"inert CLI";
    fs::write(bundle.temp.path().join(cli), bytes).unwrap();
    bundle.manifest.files.push(Artifact {
        name: cli.into(),
        sha256: hex::encode(Sha256::digest(bytes)),
        size: bytes.len() as u64,
    });
    bundle.sign();
    let source = bundle.source();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let m = vesta_update::Metadata {
        activation: None,
        schema: 1,
        purpose: "vesta-update-release-v1".into(),
        channel: "stable".into(),
        sequence: 10,
        version: bundle.manifest.version.clone(),
        target: platform::target().unwrap().into(),
        manifest_sha256: source.manifest_sha256.clone(),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 86400000,
    };
    let bytes = serde_json::to_vec(&m).unwrap();
    fs::write(bundle.temp.path().join("update.json"), &bytes).unwrap();
    fs::write(
        bundle.temp.path().join("update.sig"),
        base64::engine::general_purpose::STANDARD.encode(bundle.key.sign(&bytes).as_ref()),
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("installation");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    fs::write(root.join("bin/active-fixture"), b"unchanged").unwrap();
    let receipt = Receipt {
        schema: 1,
        installation_id: "1".repeat(32),
        answers: answers(root.clone()),
        release_version: bundle.manifest.version.clone(),
        release_manifest_sha256: source.manifest_sha256.clone(),
        phase: "installed".into(),
        tls_sha256: "2".repeat(64),
        startup_registered: false,
        external_status: vesta_setup::network::ExternalStatus::NotChecked,
    };
    engine::write_private(&root, "setup.json", &serde_json::to_vec(&receipt).unwrap()).unwrap();
    let run = |action: &str, extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vesta-setup"))
            .args(["update", action, "--root"])
            .arg(&root)
            .args(extra)
            .output()
            .unwrap()
    };
    let init = run(
        "init",
        &[
            "--bundle-dir",
            bundle.temp.path().to_str().unwrap(),
            "--release-key",
            &source.release_key,
            "--channel",
            "stable",
        ],
    );
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let status = run("status", &[]);
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["activation_supported"], false);
    assert_eq!(status["trust"]["sequence"], 10);
    assert!(status["startup_policy"].is_null());
    assert!(status["last_startup_check"].is_null());
    let configured = run(
        "configure",
        &[
            "--startup-check",
            "on",
            "--interval-seconds",
            "3600",
            "--bundle-dir",
            bundle.temp.path().to_str().unwrap(),
        ],
    );
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let configured: serde_json::Value = serde_json::from_slice(&configured.stdout).unwrap();
    assert_eq!(configured["check_on_start"], true);
    assert_eq!(configured["minimum_interval_ms"], 3600000);
    for extras in [
        vec![
            "--startup-check",
            "on",
            "--interval-seconds",
            "0",
            "--bundle-dir",
            bundle.temp.path().to_str().unwrap(),
        ],
        vec![
            "--startup-check",
            "on",
            "--interval-seconds",
            "18446744073709551615",
            "--bundle-dir",
            bundle.temp.path().to_str().unwrap(),
        ],
        vec![
            "--startup-check",
            "on",
            "--interval-seconds",
            "3600",
            "--bundle-url",
            "https://user:fake-secret@example.test/releases",
        ],
    ] {
        assert!(!run("configure", &extras).status.success());
    }
    assert!(!run("check", &["--startup-check", "on"]).status.success());
    assert!(
        !run(
            "configure",
            &[
                "--startup-check",
                "on",
                "--interval-seconds",
                "3600",
                "--bundle-dir",
                bundle.temp.path().to_str().unwrap(),
                "--channel",
                "nightly"
            ]
        )
        .status
        .success()
    );
    assert!(
        run("configure", &["--startup-check", "off"])
            .status
            .success()
    );
    let disabled = run("status", &[]);
    let disabled: serde_json::Value = serde_json::from_slice(&disabled.stdout).unwrap();
    assert_eq!(disabled["startup_policy"]["check_on_start"], false);
    let stage = run(
        "stage",
        &["--bundle-dir", bundle.temp.path().to_str().unwrap()],
    );
    assert!(
        stage.status.success(),
        "{}",
        String::from_utf8_lossy(&stage.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&stage.stdout).unwrap()["phase"],
        "staged"
    );
    let verified = run("verify", &["--manifest-sha256", &source.manifest_sha256]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let observation: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(observation["activation_supported"], false);
    assert_eq!(observation["files"], 4);
    assert_eq!(observation["manifest_sha256"], source.manifest_sha256);
    assert!(
        !run(
            "verify",
            &[
                "--manifest-sha256",
                &source.manifest_sha256,
                "--bundle-dir",
                bundle.temp.path().to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert!(
        !run("check", &["--manifest-sha256", &source.manifest_sha256])
            .status
            .success()
    );
    assert_eq!(
        fs::read(root.join("bin/active-fixture")).unwrap(),
        b"unchanged"
    );
    assert!(
        !run("stage", &["--release-key", &source.release_key])
            .status
            .success()
    );
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("private/setup.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    let blocked = run(
        "stage",
        &["--bundle-dir", bundle.temp.path().to_str().unwrap()],
    );
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("setup_already_running"));
    let blocked = run("verify", &["--manifest-sha256", &source.manifest_sha256]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("setup_already_running"));
}

#[test]
fn uninstall_rejects_confirmation_bypasses_and_ambiguous_selection_without_side_effects() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("host").to_string_lossy().into_owned();
    let registry = temporary
        .path()
        .join("registry")
        .to_string_lossy()
        .into_owned();
    for (args, error) in [
        (
            vec!["uninstall", "--root", &root, "--yes"],
            "setup_uninstall_options_invalid_no_confirmation_bypass",
        ),
        (
            vec!["uninstall", "--root", &root, "--answers", "answers.json"],
            "setup_uninstall_options_invalid_no_confirmation_bypass",
        ),
        (
            vec![
                "uninstall",
                "--root",
                &root,
                "--server",
                "owned",
                "--servers-root",
                &registry,
            ],
            "setup_server_uninstall_uses_servers_root_only",
        ),
        (vec!["uninstall"], "setup_uninstall_root_required"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vesta-setup"))
            .args(args)
            .current_dir(temporary.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(error));
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
    }
}
impl Bundle {
    fn new(real_setup: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let key = Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let mut files = Vec::new();
        for (i, stem) in ["vesta-setup", "vesta-agent", "vesta-supervisor"]
            .iter()
            .enumerate()
        {
            let name = format!("{stem}{suffix}");
            let bytes = if i == 0 && real_setup {
                fs::read(env!("CARGO_BIN_EXE_vesta-setup")).unwrap()
            } else {
                format!("fixture component {stem}").into_bytes()
            };
            fs::write(temp.path().join(&name), &bytes).unwrap();
            files.push(Artifact {
                name,
                sha256: hex::encode(Sha256::digest(&bytes)),
                size: bytes.len() as u64,
            });
        }
        let manifest = Manifest {
            schema: 1,
            version: "0.1.0".into(),
            target: platform::target().unwrap().into(),
            files,
        };
        let this = Self {
            temp,
            key,
            manifest,
        };
        this.sign();
        this
    }
    fn sign(&self) {
        let bytes = serde_json::to_vec(&self.manifest).unwrap();
        fs::write(self.temp.path().join("manifest.json"), &bytes).unwrap();
        fs::write(
            self.temp.path().join("manifest.sig"),
            base64::engine::general_purpose::STANDARD.encode(self.key.sign(&bytes).as_ref()),
        )
        .unwrap();
    }
    fn source(&self) -> Source {
        Source {
            asset_prefix: None,
            local: Some(self.temp.path().to_owned()),
            base_url: None,
            manifest_sha256: hex::encode(Sha256::digest(
                fs::read(self.temp.path().join("manifest.json")).unwrap(),
            )),
            release_key: hex::encode(self.key.public_key().as_ref()),
        }
    }
}
fn answers(root: PathBuf) -> Answers {
    Answers {
        schema: 1,
        root,
        existing_server: None,
        startup: Startup::Manual,
        port: port(),
        external_probe: None,
    }
}

#[test]
fn target_prefixed_signed_release_assets_install_to_logical_filenames() {
    let bundle = Bundle::new(false);
    let prefix = format!("{}-", platform::target().unwrap());
    let mut source = bundle.source();
    for name in ["manifest.json", "manifest.sig"]
        .iter()
        .map(|n| n.to_string())
        .chain(bundle.manifest.files.iter().map(|f| f.name.clone()))
    {
        fs::copy(
            bundle.temp.path().join(&name),
            bundle.temp.path().join(format!("{prefix}{name}")),
        )
        .unwrap();
    }
    source.asset_prefix = Some(prefix);
    let release = release::load(source).unwrap();
    let installed = tempfile::tempdir().unwrap();
    release
        .install(installed.path(), &mut |_, _, _| {})
        .unwrap();
    for artifact in &bundle.manifest.files {
        assert_eq!(
            fs::read(installed.path().join("bin").join(&artifact.name)).unwrap(),
            fs::read(bundle.temp.path().join(&artifact.name)).unwrap()
        );
    }
    let mut source = bundle.source();
    source.asset_prefix = Some("../escape-".into());
    assert!(release::load(source).is_err());
}
fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn signatures_pins_and_strict_artifact_inventory_are_checked_before_install() {
    let mut bundle = Bundle::new(false);
    release::load(bundle.source()).unwrap();
    let mut source = bundle.source();
    source.release_key = "00".repeat(32);
    assert!(release::load(source).is_err());
    let mut source = bundle.source();
    source.manifest_sha256 = "00".repeat(32);
    assert!(release::load(source).is_err());
    bundle.manifest.version = "tampered".into();
    fs::write(
        bundle.temp.path().join("manifest.json"),
        serde_json::to_vec(&bundle.manifest).unwrap(),
    )
    .unwrap();
    assert!(
        release::load(bundle.source())
            .unwrap_err()
            .contains("signature")
    );
    bundle.manifest.files[0].name = "../outside".into();
    bundle.sign();
    assert!(release::load(bundle.source()).is_err());
    bundle.manifest = Bundle::new(false).manifest;
    bundle.manifest.target = "other-platform".into();
    bundle.sign();
    assert!(release::load(bundle.source()).is_err());
}
#[test]
fn modified_verified_release_is_rejected_before_creating_any_files() {
    let bundle = Bundle::new(false);
    let mut release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("new");
    release.manifest.files[0].name = "../escape".into();
    assert!(release.install(&root, &mut |_, _, _| {}).is_err());
    assert!(engine::prepare(&answers(root.clone()), &release, &mut |_, _, _, _| {}).is_err());
    assert!(!root.exists());
}

#[test]
fn signed_short_cli_is_installed_and_unknown_extra_artifacts_are_refused() {
    let mut bundle = Bundle::new(false);
    let name = if cfg!(windows) { "vesta.exe" } else { "vesta" };
    let bytes = b"owned signed short CLI fixture";
    fs::write(bundle.temp.path().join(name), bytes).unwrap();
    bundle.manifest.files.push(Artifact {
        name: name.into(),
        sha256: hex::encode(Sha256::digest(bytes)),
        size: bytes.len() as u64,
    });
    bundle.sign();
    let release = release::load(bundle.source()).unwrap();
    let installed = tempfile::tempdir().unwrap();
    release
        .install(installed.path(), &mut |_, _, _| {})
        .unwrap();
    assert_eq!(
        fs::read(installed.path().join("bin").join(name)).unwrap(),
        bytes
    );
    bundle.manifest.files[3].name = "unknown-extra".into();
    bundle.sign();
    assert!(release::load(bundle.source()).is_err());
}
#[test]
fn installation_checks_actual_bytes_and_never_overwrites_collisions() {
    let bundle = Bundle::new(false);
    let release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    release.install(temp.path(), &mut |_, _, _| {}).unwrap();
    release.install(temp.path(), &mut |_, _, _| {}).unwrap();
    let first = temp.path().join("bin").join(&bundle.manifest.files[0].name);
    fs::write(&first, b"owner data").unwrap();
    assert!(release.install(temp.path(), &mut |_, _, _| {}).is_err());
    assert_eq!(fs::read(first).unwrap(), b"owner data");
    let root = tempfile::tempdir().unwrap();
    fs::write(
        bundle.temp.path().join(&bundle.manifest.files[0].name),
        b"altered download",
    )
    .unwrap();
    assert!(release.install(root.path(), &mut |_, _, _| {}).is_err());
    assert!(
        !root
            .path()
            .join("bin")
            .join(&bundle.manifest.files[0].name)
            .exists()
    );
}
#[test]
fn untrusted_headless_input_and_busy_port_have_no_install_effects() {
    let bundle = Bundle::new(false);
    let release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut a = answers(temp.path().join("new"));
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    a.port = socket.local_addr().unwrap().port();
    assert_eq!(
        engine::prepare(&a, &release, &mut |_, _, _, _| {}).unwrap_err(),
        "setup_port_in_use"
    );
    assert!(!a.root.exists());
    let path = temp.path().join("answers.json");
    fs::write(&path, b"{\"schema\":1,\"unexpected\":true}").unwrap();
    assert!(engine::read_answers(&path).is_err());
    let output = Command::new(env!("CARGO_BIN_EXE_vesta-setup"))
        .args([
            "--answers",
            path.to_str().unwrap(),
            "--bundle-dir",
            bundle.temp.path().to_str().unwrap(),
            "--manifest-sha256",
            &bundle.source().manifest_sha256,
            "--release-key",
            &bundle.source().release_key,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!a.root.exists());
}
#[test]
fn existing_game_is_observed_and_separate_installation_is_required() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("minecraft");
    fs::create_dir(&game).unwrap();
    fs::write(game.join("server.properties"), b"online-mode=true\n").unwrap();
    let mut a = answers(game.join("vesta"));
    a.existing_server = Some(game.clone());
    assert_eq!(
        engine::validate_answers(&a).unwrap_err(),
        "setup_existing_server_overlap"
    );
    a.root = temp.path().join("host");
    engine::validate_answers(&a).unwrap();
    assert_eq!(
        fs::read(game.join("server.properties")).unwrap(),
        b"online-mode=true\n"
    );
    assert!(!a.root.exists());
    fs::create_dir(&a.root).unwrap();
    fs::write(a.root.join("keep"), b"important").unwrap();
    assert!(engine::validate_answers(&a).is_err());
}
#[cfg(unix)]
#[test]
fn directory_links_are_refused_for_sources_and_destinations() {
    let bundle = Bundle::new(false);
    let release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("bin")).unwrap();
    assert!(release.install(temp.path(), &mut |_, _, _| {}).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    let linked = temp.path().join("linked");
    std::os::unix::fs::symlink(bundle.temp.path(), &linked).unwrap();
    let mut source = bundle.source();
    source.local = Some(linked);
    assert!(release::load(source).is_err());
}
#[cfg(windows)]
#[test]
fn windows_junction_cannot_redirect_installed_binaries() {
    let bundle = Bundle::new(false);
    let release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = temp.path().join("bin");
    let result = Command::new("cmd.exe")
        .args([
            "/c",
            "mklink",
            "/J",
            link.to_str().unwrap(),
            outside.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "creating an owned test junction failed"
    );
    assert!(release.install(temp.path(), &mut |_, _, _| {}).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    fs::remove_dir(link).unwrap();
}

#[cfg(windows)]
#[test]
#[allow(unsafe_code)] // Bounded native spelling query on a newly owned fixture.
fn receipt_accepts_native_root_aliases_but_refuses_copies_and_junctions() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let temp = tempfile::Builder::new()
        .prefix("vesta-setup-receipt-alias-")
        .tempdir_in(std::env::var_os("ProgramData").unwrap())
        .unwrap();
    let root = temp.path().join("host");
    fs::create_dir(&root).unwrap();
    let input: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut output = vec![0u16; 32_768];
    // SAFETY: terminated input and a live output of the advertised capacity.
    let count = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
            input.as_ptr(),
            output.as_mut_ptr(),
            output.len() as u32,
        )
    } as usize;
    assert!(count > 0 && count < output.len());
    let short = PathBuf::from(std::ffi::OsString::from_wide(&output[..count]));
    eprintln!("native setup root alias exercised: {}", short != root);
    assert_eq!(
        fs::canonicalize(&short).unwrap(),
        fs::canonicalize(&root).unwrap()
    );
    let receipt = Receipt {
        schema: 1,
        installation_id: "a".repeat(32),
        answers: answers(short.clone()),
        release_version: "fixture".into(),
        release_manifest_sha256: "b".repeat(64),
        phase: "installed".into(),
        tls_sha256: "c".repeat(64),
        startup_registered: false,
        external_status: Default::default(),
    };
    let bytes = serde_json::to_vec(&receipt).unwrap();
    engine::write_private(&short, "setup.json", &bytes).unwrap();
    assert_eq!(engine::read_receipt(&short).unwrap().answers.root, short);
    let loaded = engine::read_receipt(&fs::canonicalize(&root).unwrap()).unwrap();
    assert_eq!(loaded.answers.root, short);
    assert_eq!(loaded.installation_id, receipt.installation_id);
    assert_eq!(loaded.tls_sha256, receipt.tls_sha256);
    let copy = temp.path().join("copied-host");
    fs::create_dir(&copy).unwrap();
    engine::write_private(&copy, "setup.json", &bytes).unwrap();
    assert_eq!(
        engine::read_receipt(&copy).unwrap_err(),
        "setup_receipt_invalid"
    );
    let link = temp.path().join("host-link");
    let status = Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&root)
        .output()
        .unwrap();
    assert!(status.status.success(), "create owned fixture junction");
    let linked_result = engine::read_receipt(&link);
    let mut redirected = receipt.clone();
    redirected.answers.root = link.clone();
    engine::write_private(
        &root,
        "setup.json",
        &serde_json::to_vec(&redirected).unwrap(),
    )
    .unwrap();
    let redirected_result = engine::read_receipt(&root);
    engine::write_private(&root, "setup.json", &bytes).unwrap();
    fs::remove_dir(&link).unwrap();
    assert_eq!(linked_result.unwrap_err(), "setup_link_refused");
    assert_eq!(redirected_result.unwrap_err(), "setup_receipt_invalid");
    assert_eq!(fs::read(root.join("private/setup.json")).unwrap(), bytes);
    assert_eq!(fs::read(copy.join("private/setup.json")).unwrap(), bytes);
}

struct RunningHost {
    root: PathBuf,
}
impl Drop for RunningHost {
    fn drop(&mut self) {
        let _ = host::stop(&self.root);
        let deadline = Instant::now() + Duration::from_secs(4);
        while engine::read_receipt(&self.root).is_ok_and(|r| host::ready(&r))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
#[test]
fn real_pending_host_verifies_tls_resumes_identity_and_rejects_wrong_identity() {
    let mut bundle = Bundle::new(true);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let metadata = vesta_update::Metadata {
        activation: None,
        schema: 1,
        purpose: "vesta-update-release-v1".into(),
        channel: "stable".into(),
        sequence: 7,
        version: bundle.manifest.version.clone(),
        target: platform::target().unwrap().into(),
        manifest_sha256: bundle.source().manifest_sha256,
        issued_at_ms: now - 1000,
        expires_at_ms: now + 86400000,
    };
    let bytes = serde_json::to_vec(&metadata).unwrap();
    fs::write(bundle.temp.path().join("update.json"), &bytes).unwrap();
    fs::write(
        bundle.temp.path().join("update.sig"),
        base64::engine::general_purpose::STANDARD.encode(bundle.key.sign(&bytes).as_ref()),
    )
    .unwrap();
    let release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut a = answers(temp.path().join("host"));
    a.external_probe = Some(vesta_setup::network::ExternalProbe {
        url: format!("https://127.0.0.1:{}/v1/check", port()),
        consent: true,
    });
    let _running = RunningHost {
        root: a.root.clone(),
    };
    let receipt = engine::prepare(&a, &release, &mut |_, _, _, _| {}).unwrap();
    assert!(host::ready(&receipt));
    assert_eq!(receipt.phase, "ready");
    let updates = vesta_update::UpdateStore::open(&a.root.join("private/update-state")).unwrap();
    assert_eq!(updates.trust().unwrap().sequence, 7);
    assert_eq!(
        updates.trust().unwrap().release_key,
        bundle.source().release_key
    );
    assert_eq!(
        receipt.external_status,
        vesta_setup::network::ExternalStatus::Unavailable
    );
    assert!(!a.root.join("game").exists());
    let descriptor: serde_json::Value =
        serde_json::from_slice(&fs::read(a.root.join("private/handoff.json")).unwrap()).unwrap();
    assert_eq!(descriptor["status"], "awaiting_app_onboarding");
    assert_eq!(descriptor["tls_sha256"], receipt.tls_sha256);
    assert!(descriptor.get("pairing_code").is_none());
    let mut impostor = receipt.clone();
    impostor.installation_id = "a".repeat(32);
    assert!(!host::ready(&impostor));
    impostor = receipt.clone();
    impostor.tls_sha256 = "00".repeat(32);
    assert!(!host::ready(&impostor));
    let resumed = engine::prepare(&a, &release, &mut |_, _, _, _| {}).unwrap();
    assert_eq!(resumed.installation_id, receipt.installation_id);
    assert_eq!(resumed.tls_sha256, receipt.tls_sha256);
    // The same version label cannot substitute a different signed release on resume.
    bundle.manifest.files[1].sha256 = "00".repeat(32);
    bundle.sign();
    let different_release = release::load(bundle.source()).unwrap();
    assert_eq!(
        engine::prepare(&a, &different_release, &mut |_, _, _, _| {}).unwrap_err(),
        "setup_resume_settings_mismatch"
    );
    assert!(host::activate(&a.root).is_err());
    host::stop(&a.root).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while host::ready(&receipt) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!host::ready(&receipt));
    // OS-local configuration is picked up by the actual installed host on its
    // next startup, without requiring a manifest/artifact download or mutation.
    updates
        .configure_startup_check(&vesta_update::StartupPolicy {
            check_on_start: true,
            minimum_interval_ms: 3600000,
            source: vesta_update::UpdateSource {
                local: Some(bundle.temp.path().to_owned()),
                base_url: None,
                asset_prefix: None,
            },
        })
        .unwrap();
    let executable = a.root.join("bin").join(&bundle.manifest.files[0].name);
    platform::start_host(&a.root, &executable).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while !host::ready(&receipt) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        host::ready(&receipt),
        "manual host restart must consume its previous stop request"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while updates
        .last_startup_check()
        .unwrap()
        .is_none_or(|c| c.outcome == vesta_update::CheckOutcome::Incomplete)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let checked = updates
        .last_startup_check()
        .unwrap()
        .expect("installed host must execute the configured startup discovery");
    assert_eq!(checked.outcome, vesta_update::CheckOutcome::Current);
    assert_eq!(
        checked.candidate.unwrap().manifest_sha256,
        receipt.release_manifest_sha256
    );
    assert_eq!(updates.trust().unwrap().sequence, 7);
    assert!(updates.status().unwrap().is_empty());
    assert!(host::ready(&receipt));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(a.root.join("private/key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
}
#[test]
fn interrupted_key_creation_recovers_without_rotating_the_private_key() {
    let bundle = Bundle::new(true);
    let release = release::load(bundle.source()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let a = answers(temp.path().join("host"));
    fs::create_dir(&a.root).unwrap();
    let key = rcgen::KeyPair::generate().unwrap().serialize_pem();
    engine::write_private(&a.root, "key.pem", key.as_bytes()).unwrap();
    let receipt = Receipt {
        schema: 1,
        installation_id: "a".repeat(32),
        answers: a.clone(),
        release_version: release.manifest.version.clone(),
        release_manifest_sha256: release.manifest_hash().into(),
        phase: "preparing".into(),
        tls_sha256: String::new(),
        startup_registered: false,
        external_status: Default::default(),
    };
    engine::write_private(
        &a.root,
        "setup.json",
        &serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    let _running = RunningHost {
        root: a.root.clone(),
    };
    let ready = engine::prepare(&a, &release, &mut |_, _, _, _| {}).unwrap();
    assert!(host::ready(&ready));
    assert_eq!(
        fs::read(a.root.join("private/key.pem")).unwrap(),
        key.as_bytes()
    );
    assert_eq!(ready.installation_id, receipt.installation_id);
}
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut in_escape = false;
    for c in text.chars() {
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
            continue;
        }
        out.push(c);
    }
    out
}
#[test]
fn every_production_visual_state_is_bounded_and_plain_mode_has_no_escape_sequences() {
    let states = visual_states::all();
    assert!(states.len() >= 25);
    let mut names = std::collections::BTreeSet::new();
    for (id, view) in states {
        assert!(names.insert(id));
        for width in [40, 44, 80, 100] {
            let rendered = ui::render(
                &view,
                ui::Theme {
                    color: true,
                    unicode: true,
                    width,
                },
            );
            for line in strip_ansi(&rendered).lines() {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line) <= width,
                    "{id} overflows width {width}: {line:?}"
                );
            }
            let plain = ui::render(
                &view,
                ui::Theme {
                    color: false,
                    unicode: false,
                    width,
                },
            );
            assert!(!plain.contains('\x1b'));
            assert!(
                !plain.contains('╭'),
                "ASCII fallback leaked Unicode chrome in {id}"
            );
        }
    }
}
#[test]
fn pasted_terminal_controls_and_wide_characters_cannot_break_the_frame() {
    let view = ui::View {
        step: 2,
        heading: "\x1b[2J\rspoofed".into(),
        description: vec![],
        rows: vec![ui::Row::Input {
            label: "Path".into(),
            value: "世界/e\u{301}/\x1b]8;;https://invalid\x07name".repeat(20),
            error: Some("\n\tcontrol".into()),
        }],
        hint: String::new(),
    };
    let text = ui::render(
        &view,
        ui::Theme {
            color: false,
            unicode: true,
            width: 44,
        },
    );
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\r'));
    assert!(!text.contains('\x07'));
    for line in text.lines() {
        assert!(unicode_width::UnicodeWidthStr::width(line) <= 44);
    }
}
#[test]
fn input_frame_preserves_both_borders_at_every_supported_width() {
    let view = visual_states::all()
        .into_iter()
        .find(|(id, _)| *id == "location-new")
        .unwrap()
        .1;
    for width in [40, 44, 80, 88, 100] {
        let text = ui::render(
            &view,
            ui::Theme {
                color: false,
                unicode: true,
                width,
            },
        );
        let top = text.lines().find(|line| line.contains('╭')).unwrap();
        let bottom = text.lines().find(|line| line.contains('╰')).unwrap();
        assert!(top.ends_with('╮'), "{top}");
        assert!(bottom.ends_with('╯'), "{bottom}");
        assert!(!text.contains('…'));
    }
}
#[test]
fn preview_export_is_new_only_and_uses_real_renderer_output() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("frames");
    engine::export_visuals(&path).unwrap();
    let states: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(path.join("states.json")).unwrap()).unwrap();
    assert_eq!(states.len(), visual_states::all().len());
    assert!(engine::export_visuals(&path).is_err());
    let welcome = visual_states::all()
        .into_iter()
        .find(|(id, _)| *id == "welcome")
        .unwrap()
        .1;
    assert_eq!(
        fs::read_to_string(path.join("welcome.ansi")).unwrap(),
        ui::render(
            &welcome,
            ui::Theme {
                color: true,
                unicode: true,
                width: 88
            }
        )
    );
}

#[test]
fn default_and_custom_ports_are_validated_and_rechecked_against_real_listeners() {
    use vesta_setup::{network, wizard};
    let temp = tempfile::tempdir().unwrap();
    let a = answers(temp.path().join("host"));
    let mut json = serde_json::to_value(&a).unwrap();
    json.as_object_mut().unwrap().remove("port");
    let path = temp.path().join("answers.json");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert_eq!(
        engine::read_answers(&path).unwrap().port,
        network::DEFAULT_PORT
    );
    for invalid in ["0", "65536", "not a port", "-1"] {
        assert!(wizard::validate_port(invalid).is_err());
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let selected = listener.local_addr().unwrap().port().to_string();
    assert!(
        wizard::validate_port(&selected)
            .unwrap_err()
            .contains("already in use")
    );
    drop(listener);
    wizard::validate_port(&selected).unwrap();
}

#[test]
fn external_diagnostics_require_consent_bind_the_reply_and_never_turn_failure_into_setup_failure() {
    use vesta_setup::network::{
        self, ExternalProbe, ExternalStatus, ProbeClient, ProbeRequest, ProbeResponse,
    };
    struct Probe(u8);
    impl ProbeClient for Probe {
        fn check(&self, _url: &str, r: &ProbeRequest) -> Result<ProbeResponse, String> {
            if self.0 == 2 {
                return Err("unavailable".into());
            }
            let mut response = ProbeResponse {
                schema: r.schema,
                port: r.port,
                installation_id: r.installation_id.clone(),
                tls_sha256: r.tls_sha256.clone(),
                nonce: r.nonce.clone(),
                reachable: self.0 != 1,
            };
            match self.0 {
                3 => response.nonce = "0".repeat(32),
                4 => response.port = r.port.saturating_sub(1),
                5 => response.installation_id = "0".repeat(32),
                6 => response.tls_sha256 = "0".repeat(64),
                7 => response.schema = 9,
                _ => {}
            }
            Ok(response)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let mut receipt = Receipt {
        schema: 1,
        installation_id: "a".repeat(32),
        answers: answers(temp.path().join("host")),
        release_version: "0.1.0".into(),
        release_manifest_sha256: "a".repeat(64),
        phase: "ready".into(),
        tls_sha256: "b".repeat(64),
        startup_registered: false,
        external_status: ExternalStatus::NotChecked,
    };
    assert_eq!(
        network::diagnose(&receipt, &Probe(0)),
        ExternalStatus::NotChecked
    );
    let mut configuration = ExternalProbe {
        url: "https://probe.example.org/v1/check".into(),
        consent: true,
    };
    configuration.validate().unwrap();
    receipt.answers.external_probe = Some(configuration.clone());
    assert_eq!(
        network::diagnose(&receipt, &Probe(0)),
        ExternalStatus::Reachable
    );
    assert_eq!(
        network::diagnose(&receipt, &Probe(1)),
        ExternalStatus::Unreachable
    );
    for mode in 2..=7 {
        assert_eq!(
            network::diagnose(&receipt, &Probe(mode)),
            ExternalStatus::Unavailable
        );
    }
    configuration.consent = false;
    assert!(configuration.validate().is_err());
    receipt.answers.external_probe = Some(configuration.clone());
    assert_eq!(
        network::diagnose(&receipt, &Probe(0)),
        ExternalStatus::Unavailable
    );
    for url in [
        "http://probe.example.org/v1/check",
        "https://owner:secret@probe.example.org/v1/check",
        "https://probe.example.org/v1/check?token=secret",
        "https://probe.example.org/v1/check#fragment",
    ] {
        configuration.url = url.into();
        configuration.consent = true;
        assert!(configuration.validate().is_err());
    }
}

#[test]
fn setup_copy_is_concise_and_cancellation_uses_the_finish_step() {
    assert_eq!(engine::cancelled_view().step, 6);
    for (id, view) in visual_states::all() {
        let text = ui::render(
            &view,
            ui::Theme {
                color: false,
                unicode: true,
                width: 88,
            },
        )
        .to_lowercase();
        for removed in ["java", "eula", "in the app", "app onboarding"] {
            assert!(
                !text.contains(removed),
                "{id} retains unnecessary copy: {removed}"
            );
        }
    }
}

#[test]
fn paths_use_native_separators_without_reinterpreting_unix_names() {
    use std::path::Path;
    use vesta_setup::paths;
    #[cfg(windows)]
    {
        assert_eq!(
            paths::display(Path::new("C:/Vesta/host/private/handoff.json")),
            r"C:\Vesta\host\private\handoff.json"
        );
        assert_eq!(
            paths::display(Path::new(r"\\?\UNC\Host\Share\Vesta")),
            r"\\Host\Share\Vesta"
        );
        assert_eq!(
            paths::comparable(Path::new(r"\\?\C:\Vesta\host")),
            paths::comparable(Path::new("c:/vesta//host/"))
        );
    }
    #[cfg(unix)]
    {
        let path = Path::new(r"/tmp/Host\literal");
        assert_eq!(paths::normalize(path), path);
        assert_ne!(
            paths::comparable(path),
            paths::comparable(Path::new("/tmp/Host/literal"))
        );
        assert_ne!(
            paths::comparable(path),
            paths::comparable(Path::new(r"/tmp/host\literal"))
        );
    }
    assert!(
        engine::guard(Path::new(if cfg!(windows) {
            "C:/Vesta/../escape"
        } else {
            "/tmp/Vesta/../escape"
        }))
        .is_err()
    );
}
#[path = "support/native_activation.rs"]
mod native_activation;
