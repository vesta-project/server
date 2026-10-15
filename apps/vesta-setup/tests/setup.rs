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
