//! Small, bounded TLS readiness endpoint while app onboarding is pending.
//! No network mutation, owner enrollment, game launch or credential issuance.
use crate::engine::{self, Receipt, Result};
use fs2::FileExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn raw_credentials(root: &Path) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert = engine::read_private(&root.join("private/certificate.pem"), 16384)?;
    let key = engine::read_private(&root.join("private/key.pem"), 16384)?;
    let certificates: Vec<_> = CertificateDer::pem_slice_iter(&cert)
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| "setup_tls_invalid")?;
    if certificates.len() != 1 {
        return Err("setup_tls_invalid".into());
    }
    let key = PrivateKeyDer::from_pem_slice(&key).map_err(|_| "setup_tls_invalid")?;
    Ok((certificates, key))
}
fn credentials(
    root: &Path,
    receipt: &Receipt,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let (cert, key) = raw_credentials(root)?;
    if hex::encode(Sha256::digest(cert[0].as_ref())) != receipt.tls_sha256 {
        return Err("setup_tls_identity_changed".into());
    }
    Ok((cert, key))
}
pub fn identity_fingerprint(root: &Path) -> Result<String> {
    let (cert, key) = raw_credentials(root)?;
    let fingerprint = hex::encode(Sha256::digest(cert[0].as_ref()));
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| "setup_tls_invalid")?
        .with_no_client_auth()
        .with_single_cert(cert, key)
        .map_err(|_| "setup_tls_invalid")?;
    Ok(fingerprint)
}
pub fn serve(root: &Path) -> Result<()> {
    let receipt = engine::read_receipt(root)?;
    let directory = vesta_state::StateDirectory::new(root.join("private"), None)
        .map_err(|_| "setup_private_directory_refused")?;
    directory
        .prepare()
        .map_err(|_| "setup_private_directory_refused")?;
    let lock_path = root.join("private/host.lock");
    engine::guard(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|_| "setup_host_lock_failed")?;
    directory
        .tighten_file(&lock_path)
        .map_err(|_| "setup_private_permissions_failed")?;
    lock.try_lock_exclusive()
        .map_err(|_| "setup_host_already_running")?;
    if receipt.phase == "activated" {
        return run_agent(root);
    }
    // An explicit new host invocation consumes a previous clean stop request.
    // Hold the host lock first so a duplicate invocation cannot cancel a stop.
    let stop = root.join("private/stop-request");
    if stop.exists() {
        if engine::read_private(&stop, 128)? != receipt.installation_id.as_bytes() {
            return Err("setup_stop_request_invalid".into());
        }
        std::fs::remove_file(&stop).map_err(|_| "setup_restart_failed")?;
    }
    let (certificates, key) = credentials(root, &receipt)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "setup_tls_invalid")?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| "setup_tls_invalid")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, receipt.answers.port))
        .map_err(|_| "setup_port_in_use")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "setup_host_unavailable")?;
    let active = Arc::new(AtomicUsize::new(0));
    let mut checked = Instant::now();
    loop {
        if checked.elapsed() >= Duration::from_millis(250) {
            checked = Instant::now();
            let stop = root.join("private/stop-request");
            if stop.exists()
                && engine::read_private(&stop, 128)
                    .is_ok_and(|b| b == receipt.installation_id.as_bytes())
            {
                return Ok(());
            }
            if engine::read_receipt(root)?.phase == "activated" {
                drop(listener);
                let deadline = Instant::now() + Duration::from_secs(3);
                while active.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(25));
                }
                return run_agent(root);
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if active
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        (n < 4).then_some(n + 1)
                    })
                    .is_err()
                {
                    drop(stream);
                    continue;
                }
                let config = config.clone();
                let active = active.clone();
                let receipt = receipt.clone();
                std::thread::spawn(move || {
                    struct Slot(Arc<AtomicUsize>);
                    impl Drop for Slot {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::Release);
                        }
                    }
                    let _slot = Slot(active);
                    let _ = respond(stream, config, &receipt);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(_) => return Err("setup_host_unavailable".into()),
        }
    }
}
fn respond(stream: TcpStream, config: Arc<rustls::ServerConfig>, receipt: &Receipt) -> Result<()> {
    stream
        .set_nonblocking(false)
        .map_err(|_| "setup_connection_failed")?;
    stream
        .set_read_timeout(Some(Duration::from_millis(750)))
        .map_err(|_| "setup_connection_failed")?;
    stream
        .set_write_timeout(Some(Duration::from_millis(750)))
        .map_err(|_| "setup_connection_failed")?;
    let connection =
        rustls::ServerConnection::new(config).map_err(|_| "setup_connection_failed")?;
    let mut stream = rustls::StreamOwned::new(connection, stream);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut request = Vec::new();
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        if request.len() >= 4096 || Instant::now() >= deadline {
            return Err("setup_request_refused".into());
        }
        let mut chunk = [0; 512];
        let count = stream
            .read(&mut chunk)
            .map_err(|_| "setup_request_refused")?;
        if count == 0 {
            return Err("setup_request_refused".into());
        }
        request.extend_from_slice(&chunk[..count]);
    }
    let line = request.split(|b| *b == b'\n').next().unwrap_or_default();
    let (status, body) = if line == b"GET /api/v3/setup HTTP/1.1\r" {
        (
            "200 OK",
            serde_json::json!({"api_version":"3","installation_id":receipt.installation_id,"status":"awaiting_app_onboarding","version":receipt.release_version,"management_available":false}),
        )
    } else {
        ("404 Not Found", serde_json::json!({"error":"not_found"}))
    };
    let bytes = serde_json::to_vec(&body).map_err(|_| "setup_response_failed")?;
    write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n",bytes.len()).map_err(|_|"setup_response_failed")?;
    stream
        .write_all(&bytes)
        .and_then(|_| stream.flush())
        .map_err(|_| "setup_response_failed")?;
    Ok(())
}
pub fn ready(receipt: &Receipt) -> bool {
    check_ready(receipt).unwrap_or(false)
}
fn check_ready(receipt: &Receipt) -> Result<bool> {
    let root = &receipt.answers.root;
    let (certificates, _) = credentials(root, receipt)?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in certificates {
        roots.add(cert).map_err(|_| "setup_tls_invalid")?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "setup_tls_invalid")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from("localhost").map_err(|_| "setup_tls_invalid")?;
    let connection =
        rustls::ClientConnection::new(Arc::new(config), name).map_err(|_| "setup_tls_invalid")?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, receipt.answers.port));
    let socket = TcpStream::connect_timeout(&addr, Duration::from_millis(300))
        .map_err(|_| "setup_host_not_ready")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|_| "setup_host_not_ready")?;
    socket
        .set_write_timeout(Some(Duration::from_millis(500)))
        .map_err(|_| "setup_host_not_ready")?;
    let mut stream = rustls::StreamOwned::new(connection, socket);
    stream
        .write_all(b"GET /api/v3/setup HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|_| "setup_host_not_ready")?;
    let mut bytes = Vec::new();
    // The server closes its underlying socket after the response. A missing TLS
    // close_notify after the complete HTTP body is harmless for this bounded probe.
    let mut buffer = [0; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() > 4096 {
                    return Err("setup_response_invalid".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return Err("setup_host_not_ready".into()),
        }
    }
    let delimiter = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("setup_response_invalid")?;
    if !bytes.starts_with(b"HTTP/1.1 200 ") {
        return Ok(false);
    }
    let body: serde_json::Value =
        serde_json::from_slice(&bytes[delimiter + 4..]).map_err(|_| "setup_response_invalid")?;
    Ok(body["installation_id"] == receipt.installation_id
        && body["status"] == "awaiting_app_onboarding")
}
pub fn stop(root: &Path) -> Result<()> {
    let receipt = engine::read_receipt(root)?;
    if receipt.phase == "activated" {
        return Err("setup_active_agent_use_agent_stop".into());
    }
    engine::write_private(root, "stop-request", receipt.installation_id.as_bytes())
}
pub fn activate(root: &Path) -> Result<()> {
    let mut receipt = engine::read_receipt(root)?;
    if receipt.phase != "ready" || !ready(&receipt) {
        return Err("setup_pending_host_not_ready".into());
    }
    let source = root.join("host.toml");
    engine::guard(&source)?;
    let agent = root.join("bin").join(if cfg!(windows) {
        "vesta-agent.exe"
    } else {
        "vesta-agent"
    });
    engine::guard(&agent)?;
    let result = Command::new(&agent)
        .arg("check-config")
        .arg(&source)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "setup_agent_configuration_invalid")?;
    if !result.success() {
        return Err("setup_agent_configuration_invalid".into());
    }
    receipt.phase = "activated".into();
    engine::write_private(
        root,
        "setup.json",
        &serde_json::to_vec_pretty(&receipt).map_err(|_| "setup_write_failed")?,
    )
}
fn run_agent(root: &Path) -> Result<()> {
    let agent = root.join("bin").join(if cfg!(windows) {
        "vesta-agent.exe"
    } else {
        "vesta-agent"
    });
    let source = root.join("host.toml");
    engine::guard(&agent)?;
    engine::guard(&source)?;
    let result = Command::new(agent)
        .arg("serve")
        .arg(source)
        .current_dir(root)
        .stdin(Stdio::null())
        .status()
        .map_err(|_| "setup_agent_start_failed")?;
    if result.success() {
        Ok(())
    } else {
        Err("setup_agent_exited".into())
    }
}
