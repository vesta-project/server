//! Explicit native acceptance: actual locally built Vesta processes, inert game files.
//! No game starts; fixture account binding replaces only Mojang verification.
use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::{
    io::{Read, Write},
    sync::Arc,
};
use vesta_control::{
    identity::{BuiltinRole, management::session_message},
    onboarding::FirstServerSetup,
};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

fn native_bundle(dir: &std::path::Path, version: &str, bad_agent: bool) -> Bundle {
    let mut bundle = Bundle::new(false);
    bundle.manifest.version = version.into();
    bundle.manifest.files.clear();
    for stem in ["vesta-setup", "vesta-agent", "vesta-supervisor", "vesta"] {
        let name = if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.into()
        };
        let from = if bad_agent && stem == "vesta-agent" {
            if cfg!(windows) {
                "vesta-setup.exe"
            } else {
                "vesta-setup"
            }
        } else {
            &name
        };
        let bytes = fs::read(dir.join(from)).expect("explicit native component build required");
        fs::write(bundle.temp.path().join(&name), &bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                bundle.temp.path().join(&name),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        bundle.manifest.files.push(Artifact {
            name,
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        });
    }
    bundle.sign();
    bundle
}
fn metadata(bundle: &Bundle, sequence: u64) {
    let now = now();
    let input = vesta_update::Metadata {
        schema: 1,
        purpose: "vesta-update-release-v1".into(),
        channel: "stable".into(),
        sequence,
        version: bundle.manifest.version.clone(),
        target: platform::target().unwrap().into(),
        manifest_sha256: bundle.source().manifest_sha256,
        issued_at_ms: now - 1000,
        expires_at_ms: now + 86400000,
        activation: Some(vesta_update::ActivationCompatibility {
            protocol: 1,
            state_schema_min: vesta_state::CURRENT_SCHEMA_VERSION,
            state_schema_max: vesta_state::CURRENT_SCHEMA_VERSION,
            supervisor_ipc: 1,
        }),
    };
    let bytes = serde_json::to_vec(&input).unwrap();
    fs::write(bundle.temp.path().join("update.json"), &bytes).unwrap();
    fs::write(
        bundle.temp.path().join("update.sig"),
        base64::engine::general_purpose::STANDARD.encode(bundle.key.sign(&bytes).as_ref()),
    )
    .unwrap();
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn executable(root: &std::path::Path, stem: &str) -> PathBuf {
    root.join("bin").join(if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.into()
    })
}
fn try_tls(
    receipt: &Receipt,
    method: &str,
    path: &str,
    body: &serde_json::Value,
    token: Option<&str>,
) -> Result<(u16, serde_json::Value), &'static str> {
    use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
    let cert = fs::read(receipt.answers.root.join("private/certificate.pem"))
        .map_err(|_| "fixture certificate")?;
    let certificates = CertificateDer::pem_slice_iter(&cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "fixture certificate")?;
    assert_eq!(
        hex::encode(Sha256::digest(certificates[0].as_ref())),
        receipt.tls_sha256
    );
    let mut roots = rustls::RootCertStore::empty();
    for certificate in certificates {
        roots.add(certificate).map_err(|_| "fixture certificate")?;
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "fixture TLS")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connection =
        rustls::ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
            .map_err(|_| "fixture TLS")?;
    let socket = std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], receipt.answers.port).into(),
        Duration::from_secs(2),
    )
    .map_err(|_| "fixture TCP unavailable")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .map_err(|_| "fixture socket")?;
    socket
        .set_write_timeout(Some(Duration::from_secs(4)))
        .map_err(|_| "fixture socket")?;
    let mut stream = rustls::StreamOwned::new(connection, socket);
    let bytes = if method == "GET" || body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).unwrap()
    };
    let authorization = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{authorization}\r\n",bytes.len()).map_err(|_|"fixture TLS write")?;
    stream.write_all(&bytes).map_err(|_| "fixture TLS write")?;
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                output.extend_from_slice(&buffer[..n]);
                if output.len() >= 65536 {
                    return Err("fixture response bound");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return Err("fixture TLS read"),
        }
    }
    let split = output
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("fixture response framing")?;
    let status = std::str::from_utf8(&output[..split])
        .map_err(|_| "fixture response framing")?
        .split_whitespace()
        .nth(1)
        .ok_or("fixture response status")?
        .parse()
        .map_err(|_| "fixture response status")?;
    Ok((
        status,
        serde_json::from_slice(&output[split + 4..]).map_err(|_| "fixture response JSON")?,
    ))
}
fn tls(
    receipt: &Receipt,
    method: &str,
    path: &str,
    body: &serde_json::Value,
    token: Option<&str>,
) -> (u16, serde_json::Value) {
    try_tls(receipt, method, path, body, token).expect("owned pinned TLS request")
}

fn wait<F: FnMut() -> bool>(mut condition: F, label: &str) {
    let until = Instant::now() + Duration::from_secs(35);
    while !condition() {
        assert!(Instant::now() < until, "owned timeout: {label}");
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn session(receipt: &Receipt, device: &str, key: &Ed25519KeyPair) -> String {
    let (code, reply) = tls(
        receipt,
        "POST",
        "/api/v3/manage/session/challenge",
        &serde_json::json!({"device_id":device}),
        None,
    );
    assert_eq!(code, 200);
    let message = session_message(
        reply["server_id"].as_str().unwrap(),
        reply["nonce"].as_str().unwrap(),
    );
    let (code, reply) = tls(
        receipt,
        "POST",
        "/api/v3/manage/session",
        &serde_json::json!({"device_id":device,"nonce":reply["nonce"],"signature":URL_SAFE_NO_PAD.encode(key.sign(&message).as_ref())}),
        None,
    );
    assert_eq!(code, 200);
    reply["token"].as_str().unwrap().into()
}
struct Running {
    root: PathBuf,
    child: Option<std::process::Child>,
    active: bool,
}
impl Running {
    fn start(root: &std::path::Path) -> Self {
        let log = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(root.join(format!("native-host-{}.log", uuid::Uuid::new_v4())))
            .unwrap();
        let child = Command::new(executable(root, "vesta-setup"))
            .args(["host", "--root"])
            .arg(root)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        Self {
            root: root.into(),
            child: Some(child),
            active: true,
        }
    }
    fn stop(&mut self) {
        if self.root.join("host.toml").exists() {
            let status = Command::new(executable(&self.root, "vesta"))
                .current_dir(&self.root)
                .arg("shutdown")
                .arg(self.root.join("host.toml"))
                .status()
                .unwrap();
            assert!(status.success());
        } else {
            host::stop(&self.root).unwrap();
        }
        if let Some(child) = self.child.as_mut() {
            wait(
                || child.try_wait().unwrap().is_some(),
                "graceful owned host stop",
            );
            assert!(child.wait().unwrap().success());
        }
        self.child = None;
        stopped(&self.root);
        self.active = false;
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if self.active {
            // Exact trusted fixture build can stop its local IPC even while a bootstrap
            // update lock is busy. This is test cleanup, not production dispatch policy.
            let direct = PathBuf::from(std::env::var_os("VESTA_NATIVE_COMPONENTS_DIR").unwrap())
                .join(if cfg!(windows) { "vesta.exe" } else { "vesta" });
            let _ = Command::new(direct)
                .current_dir(&self.root)
                .arg("shutdown")
                .arg(self.root.join("host.toml"))
                .status();
            let _ = host::stop(&self.root);
        }
    }
}
fn cli(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    let owned_root = root.to_owned();
    let owned_args: Vec<_> = args.iter().map(|a| a.to_string()).collect();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let output = Command::new(executable(&owned_root, "vesta-setup"))
            .current_dir(&owned_root)
            .args(owned_args)
            .arg("--root")
            .arg(&owned_root)
            .output();
        let _ = tx.send(output);
    });
    let result = rx.recv_timeout(Duration::from_secs(35));
    if result.is_err() {
        // Failure cleanup only: gracefully stop this exact owned fixture so a
        // regressed inherited pipe can close. Never kill a process or mask timeout.
        let controller = PathBuf::from(std::env::var_os("VESTA_NATIVE_COMPONENTS_DIR").unwrap())
            .join(if cfg!(windows) { "vesta.exe" } else { "vesta" });
        let _ = Command::new(controller)
            .current_dir(root)
            .arg("shutdown")
            .arg(root.join("host.toml"))
            .status();
        let _ = rx.recv_timeout(Duration::from_secs(5));
        panic!("captured CLI exceeded35s; detached host must not retain capture handles");
    }
    reader.join().unwrap();
    result.unwrap().unwrap()
}

fn stopped(root: &std::path::Path) {
    use fs2::FileExt;
    wait(
        || {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join("private/host.lock"))
                .unwrap();
            file.try_lock_exclusive().is_ok()
        },
        "host lease release",
    );
}

#[test]
#[ignore = "Explicit four-component native build and owned fixture root required; never starts Minecraft"]
fn native_copy_onboarding_signed_update_and_failed_agent_recovery_preserve_identity() {
    let dir = PathBuf::from(
        std::env::var_os("VESTA_NATIVE_COMPONENTS_DIR")
            .expect("set to explicit final-source native build"),
    );
    let parent = PathBuf::from(
        std::env::var_os("VESTA_NATIVE_FIXTURE_PARENT")
            .expect("set to explicitly owned audit root"),
    );
    assert_eq!(
        fs::read_to_string(parent.join("OWNED-BY-VESTA-AUDIT"))
            .unwrap()
            .trim(),
        "remaining-backend-20261110"
    );
    let temp = tempfile::Builder::new()
        .prefix("native-activation-")
        .tempdir_in(&parent)
        .unwrap()
        .keep();
    println!("owned native evidence root: {}", temp.display());
    fs::write(temp.join("ownership.json"),serde_json::to_vec_pretty(&serde_json::json!({"owner":"remaining-backend-20261110","purpose":"native onboarding/update fixture; inert game; loopback only","root":temp,"components":dir})).unwrap()).unwrap();
    let source = temp.join("copied-export");
    fs::create_dir(&source).unwrap();
    for (name, bytes) in [
        ("server.jar", b"inert never executed".as_slice()),
        ("eula.txt", b"eula=false\n"),
        ("world/level.dat", b"world witness"),
        ("config/owned.toml", b"config witness"),
        ("world/datapacks/renamed.zip.disabled", b"disabled witness"),
    ] {
        let path = source.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let baseline = native_bundle(&dir, "1.0.0-owned", false);
    metadata(&baseline, 10);
    let release = release::load(baseline.source()).unwrap();
    let root = temp.join("installation");
    let mut a = answers(root.clone());
    a.existing_server = Some(source.clone());
    let receipt = engine::prepare(&a, &release, &mut |_, _, _, _| {}).unwrap();
    host::stop(&root).unwrap();
    wait(|| !host::ready(&receipt), "initial pending stop");
    stopped(&root);
    let java = temp.join(if cfg!(windows) { "java.exe" } else { "java" });
    fs::copy(
        dir.join(if cfg!(windows) { "vesta.exe" } else { "vesta" }),
        &java,
    )
    .unwrap();
    host::configure_server(&root, java, Some("server.jar".into()), None, true).unwrap();
    let (enrolled, key, server_id) = seed_account(&root, &receipt);
    let mut running = Running::start(&root);
    wait(|| host::ready(&receipt), "configured pending ready");
    let token = session(&receipt, &enrolled.device_id, &key);
    assert_eq!(
        tls(
            &receipt,
            "POST",
            "/api/v3/setup/claim",
            &serde_json::Value::Null,
            Some(&token)
        )
        .0,
        200
    );
    let (code, job) = tls(
        &receipt,
        "POST",
        "/api/v3/setup/plan",
        &serde_json::json!({"kind":"adopt_copy","name":"Owned native copy","minecraft_version":"1.21.1"}),
        Some(&token),
    );
    assert_eq!(code, 202);
    let id = job["id"].as_str().unwrap();
    let path = format!("/api/v3/setup/jobs/{id}");
    let mut review = serde_json::Value::Null;
    wait(
        || {
            let (code, j) = tls(
                &receipt,
                "GET",
                &path,
                &serde_json::Value::Null,
                Some(&token),
            );
            assert_eq!(code, 200);
            assert_ne!(j["state"], "failed", "owned preparation failed");
            let ready = j["state"] == "review";
            review = j;
            ready
        },
        "copy review",
    );
    let input = serde_json::json!({"job_id":id,"confirmation":review["confirmation"],"idempotency_key":"owned-native-copy","acknowledge_warnings":true,"acknowledge_legacy_checksum":false});
    assert_eq!(
        tls(
            &receipt,
            "POST",
            "/api/v3/setup/apply",
            &input,
            Some(&token)
        )
        .0,
        202
    );
    wait(
        || {
            try_tls(
                &receipt,
                "GET",
                "/api/v3/status",
                &serde_json::Value::Null,
                None,
            )
            .is_ok_and(|r| r.0 == 200)
        },
        "managed TLS bind",
    );
    let (code, status) = tls(
        &receipt,
        "GET",
        "/api/v3/status",
        &serde_json::Value::Null,
        None,
    );
    assert_eq!(code, 200);
    assert_eq!(status["server_id"], server_id);
    assert_eq!(engine::read_receipt(&root).unwrap().phase, "activated");
    let _ = session(&receipt, &enrolled.device_id, &key);
    running.stop();
    stopped(&root);
    assert!(!root.join("operator-session-pending").exists());
    let good = native_bundle(&dir, "1.0.1-owned", false);
    metadata(&good, 11);
    let output = cli(
        &root,
        &[
            "update",
            "stage",
            "--bundle-dir",
            good.temp.path().to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "stage failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = cli(
        &root,
        &[
            "update",
            "activate",
            "--manifest-sha256",
            &good.source().manifest_sha256,
            "--idempotency-key",
            "owned-good",
            "--restart",
            "off",
        ],
    );
    assert!(
        output.status.success(),
        "activation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let accepted: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(accepted["state"], "restart_pending");
    let mut running = Running::start(&root);
    let store = vesta_update::UpdateStore::open(&root.join("private/update-state")).unwrap();
    wait(
        || {
            store
                .activation_status(accepted["id"].as_str().unwrap())
                .unwrap()
                .state
                == "healthy"
        },
        "candidate native readiness commit",
    );
    assert_eq!(
        tls(
            &receipt,
            "GET",
            "/api/v3/status",
            &serde_json::Value::Null,
            None
        )
        .1["server_id"],
        server_id
    );
    let _ = session(&receipt, &enrolled.device_id, &key);
    running.stop();
    stopped(&root);
    let bad = native_bundle(&dir, "1.0.2-owned", true);
    metadata(&bad, 12);
    let output = cli(
        &root,
        &[
            "update",
            "stage",
            "--bundle-dir",
            bad.temp.path().to_str().unwrap(),
        ],
    );
    assert!(output.status.success());
    let output = cli(
        &root,
        &[
            "update",
            "activate",
            "--manifest-sha256",
            &bad.source().manifest_sha256,
            "--idempotency-key",
            "owned-bad-agent",
            "--restart",
            "on",
        ],
    );
    assert!(
        output.status.success(),
        "bad-agent preflight failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let failed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failed["state"], "restart_pending");
    let mut running = Running {
        root: root.clone(),
        child: None,
        active: true,
    };
    wait(
        || {
            store
                .activation_status(failed["id"].as_str().unwrap())
                .unwrap()
                .state
                == "recovered"
        },
        "failed candidate exact prior recovery",
    );
    wait(
        || {
            try_tls(
                &receipt,
                "GET",
                "/api/v3/status",
                &serde_json::Value::Null,
                None,
            )
            .is_ok_and(|r| r.0 == 200)
        },
        "fallback actual TLS bind",
    );
    assert_eq!(
        tls(
            &receipt,
            "GET",
            "/api/v3/status",
            &serde_json::Value::Null,
            None
        )
        .1["server_id"],
        server_id
    );
    let _ = session(&receipt, &enrolled.device_id, &key);
    running.stop();
    stopped(&root);
    assert_eq!(
        store.trust().unwrap().sequence,
        12,
        "recovery never lowers accepted floor"
    );
    assert_eq!(
        store
            .active_payload(&receipt.installation_id, now())
            .unwrap()
            .unwrap()
            .manifest_sha256(),
        good.source().manifest_sha256
    );
    for (name, bytes) in [
        ("world/level.dat", b"world witness".as_slice()),
        ("config/owned.toml", b"config witness"),
        ("world/datapacks/renamed.zip.disabled", b"disabled witness"),
        ("eula.txt", b"eula=false\n"),
    ] {
        assert_eq!(fs::read(source.join(name)).unwrap(), bytes);
        assert_eq!(
            fs::read(root.join("servers/first/game").join(name)).unwrap(),
            bytes
        );
    }
    let db = StateDatabase::open(
        StateDirectory::new(root.join("private/state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    assert_eq!(db.server_id(), server_id);
    assert_eq!(
        db.read(|c| c
            .query_row(
                "SELECT state FROM setup_provisioning WHERE id=?",
                [id],
                |r| r.get::<_, String>(0)
            )
            .map_err(Into::into))
            .unwrap(),
        "complete"
    );
    db.close().unwrap();
    println!(
        "native copy, signed candidate readiness, exact prior recovery and account/file continuity passed; no Minecraft start"
    );
}

fn seed_account(
    root: &std::path::Path,
    receipt: &Receipt,
) -> (
    vesta_control::identity::management::Enrolled,
    Ed25519KeyPair,
    String,
) {
    let db = StateDatabase::open(
        StateDirectory::new(root.join("private/state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let setup = FirstServerSetup::reserve_local(db.clone(), &receipt.installation_id).unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[57; 32]).unwrap();
    let invitation = setup
        .management()
        .issue_local(BuiltinRole::Owner, 10)
        .unwrap();
    let enrolled = setup
        .management()
        .enroll(
            invitation.secret.expose(),
            "Owned native account fixture",
            &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
        )
        .unwrap();
    // Test-only Mojang substitution: no constructor/bypass exposed to remote users.
    let account = "11111111-2222-4333-8444-555555555555";
    db.write(|c|{c.execute("INSERT INTO user_minecraft_accounts(server_id,minecraft_uuid,principal_id) VALUES(?,?,?)",(&enrolled.server_id,account,&enrolled.principal_id))?;c.execute("UPDATE management_device_keys SET linked_minecraft_uuid=? WHERE device_id=?",(account,&enrolled.device_id))?;Ok(())}).unwrap();
    let server_id = db.server_id().to_owned();
    drop(setup);
    db.close().unwrap();
    drop(db);
    (enrolled, key, server_id)
}

#[test]
#[ignore = "Explicit native build, retained audit Java21 and bounded public Mojang download; never starts Minecraft"]
fn native_pending_install_uses_actual_mojang_provider_and_hands_off_without_accepting_eula() {
    let dir = PathBuf::from(
        std::env::var_os("VESTA_NATIVE_COMPONENTS_DIR")
            .expect("explicit final-source build required"),
    );
    let java = PathBuf::from(
        std::env::var_os("VESTA_NATIVE_JAVA21").expect("explicit retained audit Java21 required"),
    );
    assert!(java.is_file());
    let parent = PathBuf::from(std::env::var_os("VESTA_NATIVE_FIXTURE_PARENT").unwrap());
    assert_eq!(
        fs::read_to_string(parent.join("OWNED-BY-VESTA-AUDIT"))
            .unwrap()
            .trim(),
        "remaining-backend-20261110"
    );
    let temp = tempfile::Builder::new()
        .prefix("native-new-server-")
        .tempdir_in(&parent)
        .unwrap()
        .keep();
    println!("owned new-server evidence root: {}", temp.display());
    fs::write(temp.join("ownership.json"),serde_json::to_vec_pretty(&serde_json::json!({"owner":"remaining-backend-20261110","purpose":"native first-server Mojang1.21.1 install; no Minecraft execution","root":temp,"components":dir})).unwrap()).unwrap();
    let bundle = native_bundle(&dir, "1.0.0-owned", false);
    metadata(&bundle, 10);
    let release = release::load(bundle.source()).unwrap();
    let root = temp.join("installation");
    let a = answers(root.clone());
    let receipt = engine::prepare(&a, &release, &mut |_, _, _, _| {}).unwrap();
    host::stop(&root).unwrap();
    wait(|| !host::ready(&receipt), "pending stop");
    stopped(&root);
    host::configure_server(&root, java, None, None, false).unwrap();
    let (enrolled, key, server_id) = seed_account(&root, &receipt);
    let mut running = Running::start(&root);
    wait(|| host::ready(&receipt), "pending configured runtime");
    let token = session(&receipt, &enrolled.device_id, &key);
    assert_eq!(
        tls(
            &receipt,
            "POST",
            "/api/v3/setup/claim",
            &serde_json::Value::Null,
            Some(&token)
        )
        .0,
        200
    );
    let started = Instant::now();
    let (code, job) = tls(
        &receipt,
        "POST",
        "/api/v3/setup/plan",
        &serde_json::json!({"kind":"install","name":"Owned vanilla install","software":"vanilla","version":"1.21.1","build":null,"loader_version":null,"experimental":false}),
        Some(&token),
    );
    assert_eq!(code, 202);
    let id = job["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v3/setup/jobs/{id}");
    let until = Instant::now() + Duration::from_secs(180);
    let review = loop {
        let (code, job) = tls(
            &receipt,
            "GET",
            &path,
            &serde_json::Value::Null,
            Some(&token),
        );
        assert_eq!(code, 200);
        assert_ne!(
            job["state"], "failed",
            "owned Mojang preparation failed: phase={} code={}",
            job["phase"], job["error"]
        );
        if job["state"] == "review" {
            break job;
        }
        assert!(Instant::now() < until, "owned preparation deadline");
        std::thread::sleep(Duration::from_millis(1000));
    };
    assert_eq!(review["review"]["minecraft_version"], "1.21.1");
    assert_eq!(review["review"]["software"], "vanilla");
    assert!(
        review["review"]["bytes"].as_u64().unwrap() < 128 * 1024 * 1024,
        "bounded fixture download budget"
    );
    let preparation_ms = started.elapsed().as_millis();
    let apply_start = Instant::now();
    assert_eq!(tls(&receipt,"POST","/api/v3/setup/apply",&serde_json::json!({"job_id":id,"confirmation":review["confirmation"],"idempotency_key":"owned-native-mojang","acknowledge_warnings":true,"acknowledge_legacy_checksum":true}),Some(&token)).0,202);
    let until = Instant::now() + Duration::from_secs(180);
    loop {
        if try_tls(
            &receipt,
            "GET",
            "/api/v3/status",
            &serde_json::Value::Null,
            None,
        )
        .is_ok_and(|(code, status)| code == 200 && status["server_id"] == server_id)
        {
            break;
        }
        if let Ok((code, job)) = try_tls(
            &receipt,
            "GET",
            &path,
            &serde_json::Value::Null,
            Some(&token),
        ) {
            if code == 200 {
                assert_ne!(
                    job["state"], "failed",
                    "owned Mojang installation failed: phase={} code={}",
                    job["phase"], job["error"]
                );
            } else {
                assert_eq!(
                    code, 404,
                    "pending route may retire only during identity-pinned managed handoff"
                );
            }
        }
        assert!(Instant::now() < until, "owned install deadline");
        std::thread::sleep(Duration::from_secs(1));
    }
    wait(
        || {
            try_tls(
                &receipt,
                "GET",
                "/api/v3/status",
                &serde_json::Value::Null,
                None,
            )
            .is_ok_and(|r| r.0 == 200)
        },
        "real installed agent",
    );
    assert_eq!(
        tls(
            &receipt,
            "GET",
            "/api/v3/status",
            &serde_json::Value::Null,
            None
        )
        .1["server_id"],
        server_id
    );
    let _ = session(&receipt, &enrolled.device_id, &key);
    running.stop();
    stopped(&root);
    let db = StateDatabase::open(
        StateDirectory::new(root.join("private/state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let raw = db
        .read(|c| {
            c.query_row(
                "SELECT completion FROM setup_provisioning WHERE id=? AND state='complete'",
                [&id],
                |r| r.get::<_, String>(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    let completion: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let artifacts = completion["artifacts"].as_array().unwrap();
    assert!(!artifacts.is_empty());
    for artifact in artifacts {
        let bytes = fs::read(artifact["path"].as_str().unwrap()).unwrap();
        assert_eq!(bytes.len() as u64, artifact["size"].as_u64().unwrap());
        assert_eq!(hex::encode(Sha256::digest(bytes)), artifact["sha256"]);
    }
    assert_eq!(
        fs::read(root.join("servers/first/game/eula.txt")).unwrap(),
        b"# Read https://www.minecraft.net/eula before accepting.\neula=false\n"
    );
    assert!(!root.join("servers/first/game/world").exists());
    assert!(!root.join("operator-session-pending").exists());
    db.close().unwrap();
    fs::write(temp.join("result.json"),serde_json::to_vec_pretty(&serde_json::json!({"provider":"mojang","minecraft":"1.21.1","preparation_ms":preparation_ms,"apply_and_handoff_ms":apply_start.elapsed().as_millis(),"review":review["review"],"artifacts":artifacts,"game_started":false,"microsoft_verification":"fixture binding"})).unwrap()).unwrap();
    println!(
        "native Mojang preparation={}ms apply/handoff={}ms; exact artifacts, completed DB, EULA false; no game start",
        preparation_ms,
        apply_start.elapsed().as_millis()
    );
}
