//! Pending first-server onboarding using canonical account authority and hardened TLS.
//! No game process, machine-wide authority, or remote setup-code issuance.
use crate::engine::{self, Receipt, Result};
use fs2::FileExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

fn raw_credentials(root: &Path) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert = engine::read_private(&root.join("private").join("certificate.pem"), 16384)?;
    let key = engine::read_private(&root.join("private").join("key.pem"), 16384)?;
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
pub(crate) struct HostLease {
    pub(crate) root: std::path::PathBuf,
    _file: std::fs::File,
}
pub(crate) fn acquire_host_lease(root: &Path) -> Result<HostLease> {
    engine::guard(&root.join("private/uninstall-pending"))?;
    engine::read_receipt(root)?;
    let directory = vesta_state::StateDirectory::new(root.join("private"), None)
        .map_err(|_| "setup_private_directory_refused")?;
    directory
        .prepare()
        .map_err(|_| "setup_private_directory_refused")?;
    let lock_path = root.join("private").join("host.lock");
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
    if root.join("private/uninstall-pending").exists() {
        return Err("setup_uninstall_pending".into());
    }
    Ok(HostLease {
        root: root.to_owned(),
        _file: lock,
    })
}
pub fn serve(root: &Path) -> Result<()> {
    if crate::activation::delegate_host(root)?.is_some() {
        return Ok(());
    }
    engine::guard(&root.join("private/uninstall-pending"))?;
    if root.join("private/uninstall-pending").exists() {
        return Err("setup_uninstall_pending".into());
    }
    let receipt = engine::read_receipt(root)?;
    let lease = acquire_host_lease(root)?;
    crate::updates::start_check(root, &receipt.release_manifest_sha256);
    if receipt.phase == "activated" {
        return run_agent(root, &lease);
    }
    // An explicit new host invocation consumes a previous clean stop request.
    // Hold the host lock first so a duplicate invocation cannot cancel a stop.
    let stop = root.join("private").join("stop-request");
    if stop.exists() {
        if engine::read_private(&stop, 128)? != receipt.installation_id.as_bytes() {
            return Err("setup_stop_request_invalid".into());
        }
        std::fs::remove_file(&stop).map_err(|_| "setup_restart_failed")?;
    }
    credentials(root, &receipt)?;
    let state = vesta_state::StateDirectory::new(root.join("private/state"), None)
        .map_err(|_| "setup_state_invalid")?;
    let db = vesta_state::StateDatabase::open(state, Arc::new(vesta_state::SystemClock))
        .map_err(|_| "setup_state_unavailable")?;
    let setup = vesta_control::onboarding::FirstServerSetup::reserve_local(
        db.clone(),
        &receipt.installation_id,
    )
    .map_err(|_| "setup_identity_unavailable")?;
    let local = root.join("private/first-server.json");
    let provisioning = if local.exists() {
        let input = serde_json::from_slice(&engine::read_private(&local, 16384)?)
            .map_err(|_| "setup_local_configuration_invalid")?;
        Some(
            vesta_control::onboarding::provisioning::Provisioning::open(
                setup.clone(),
                root,
                input,
                receipt.answers.existing_server.as_deref(),
                receipt.answers.port,
            )
            .map_err(|_| "setup_provisioning_unavailable")?,
        )
    } else {
        None
    };
    let mut pending = vesta_transport::pending::PendingHost::new(setup)
        .map_err(|_| "setup_identity_unavailable")?;
    if let Some(service) = &provisioning {
        pending = pending.with_provisioning(service.clone());
    }
    let listener = vesta_control::config::TlsListener::pending_local(
        receipt.answers.port,
        root.join("private/certificate.pem"),
        root.join("private/key.pem"),
    )
    .map_err(|_| "setup_listener_invalid")?;
    let transport =
        vesta_transport::Transport::start_pending_pinned(pending, &listener, &receipt.tls_sha256)
            .map_err(|_| "setup_host_unavailable")?;
    loop {
        if let Some(service) = &provisioning {
            let completed=service.publish_ready(|id,server_id,host,launch|{
                let consumer=serde_json::to_vec(&serde_json::json!({"schema":1,"installation_id":receipt.installation_id,"server_id":server_id,"job_id":id,"configuration":"host.toml","state":"private/state","launch_sha256":hex::encode(Sha256::digest(launch.as_bytes())),"host_sha256":hex::encode(Sha256::digest(host.as_bytes())),"installed_manifest_sha256":receipt.release_manifest_sha256})).map_err(|_|vesta_control::identity::management::permissions::PermissionError::Unavailable)?;
                publish_document(root,"private/launch.toml",launch.as_bytes()).and_then(|_|publish_document(root,"host.toml",host.as_bytes())).and_then(|_|publish_document(root,"private/consumer.json",&consumer)).map_err(|_|vesta_control::identity::management::permissions::PermissionError::Unavailable)
            }).map_err(|_|"setup_publication_incomplete")?;
            if completed {
                // Files/consumer and DB completion precede the start-admitting
                // receipt. A lost receipt write is retried from complete state.
                let mut activated = engine::read_receipt(root)?;
                activated.phase = "activated".into();
                engine::write_private(
                    root,
                    "setup.json",
                    &serde_json::to_vec(&activated).map_err(|_| "setup_write_failed")?,
                )?;
            }
        }
        let stopping = root.join("private/stop-request");
        let stop = stopping.exists()
            && engine::read_private(&stopping, 128)
                .is_ok_and(|b| b == receipt.installation_id.as_bytes());
        let activated = engine::read_receipt(root)?.phase == "activated";
        if stop || activated {
            match transport
                .shutdown(Duration::from_secs(5))
                .map_err(|_| "setup_shutdown_failed")?
            {
                vesta_transport::TransportShutdown::Complete => (),
                vesta_transport::TransportShutdown::TimedOut(_) => {
                    return Err("setup_shutdown_incomplete".into());
                }
            }
            if let Some(service) = &provisioning {
                service
                    .shutdown()
                    .map_err(|_| "setup_provisioning_shutdown_failed")?;
            }
            db.close().map_err(|_| "setup_state_busy")?;
            return if activated {
                run_agent(root, &lease)
            } else {
                Ok(())
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Explicit OS-local runtime/entrypoint selection. Requires restarting a pending
/// host so the listener's composition is immutable for its lifetime.
pub fn configure_server(
    root: &Path,
    java: std::path::PathBuf,
    adoption_jar: Option<String>,
    adoption_argfile: Option<String>,
    copied_export_confirmed: bool,
) -> Result<()> {
    let receipt = engine::read_receipt(root)?;
    if receipt.phase == "activated" {
        return Err("setup_already_activated_use_app".into());
    }
    let _lock = engine::lock_existing_installation(root)?;
    engine::guard(&java)?;
    if !java.is_file()
        || [adoption_jar.as_ref(), adoption_argfile.as_ref()]
            .into_iter()
            .flatten()
            .any(|p| vesta_control::files::RelativePath::parse(p).is_err())
        || adoption_jar.is_some() && adoption_argfile.is_some()
    {
        return Err("setup_local_configuration_invalid".into());
    }
    if (adoption_jar.is_some() || adoption_argfile.is_some())
        && receipt.answers.existing_server.is_none()
    {
        return Err("setup_copied_export_not_selected".into());
    }
    if receipt.answers.existing_server.is_some()
        && (!copied_export_confirmed || (adoption_jar.is_none() && adoption_argfile.is_none()))
    {
        return Err("setup_confirm_copied_export_required".into());
    }
    let config = vesta_control::onboarding::provisioning::LocalConfiguration {
        java,
        adoption_jar,
        adoption_argfile,
        copied_export_confirmed,
    };
    engine::write_private(
        root,
        "first-server.json",
        &serde_json::to_vec(&config).map_err(|_| "setup_write_failed")?,
    )
}

/// Generated documents publish absent-only, or verify an identical prior effect.
/// A foreign config is never overwritten, including after a lost DB commit.
fn publish_document(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    if !matches!(
        relative,
        "host.toml" | "private/launch.toml" | "private/consumer.json"
    ) {
        return Err("setup_file_invalid".into());
    }
    let target = root.join(relative);
    engine::guard(&target)?;
    if target.exists() {
        return if engine::read_private(&target, 65536)? == bytes {
            Ok(())
        } else {
            Err("setup_publication_conflict".into())
        };
    }
    let stage = root.join("private").join(format!(
        ".setup-publication-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stage)
            .map_err(|_| "setup_write_failed")?;
        let private = vesta_state::StateDirectory::new(root.join("private"), None)
            .map_err(|_| "setup_private_directory_refused")?;
        private
            .tighten_file(&stage)
            .map_err(|_| "setup_private_permissions_failed")?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "setup_write_failed")?;
        drop(file);
        engine::guard(&target)?;
        match std::fs::hard_link(&stage, &target) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if engine::read_private(&target, 65536)? != bytes {
                    return Err("setup_publication_conflict".into());
                }
            }
            Err(_) => return Err("setup_write_failed".into()),
        }
        #[cfg(unix)]
        std::fs::File::open(target.parent().ok_or("setup_path_invalid")?)
            .and_then(|d| d.sync_all())
            .map_err(|_| "setup_write_failed")?;
        Ok(())
    })();
    let cleanup = std::fs::remove_file(&stage);
    if result.is_ok() && cleanup.is_err() {
        return Err("setup_publication_cleanup_failed".into());
    }
    result
}

/// Explicit OS-local command only; never served by the pending HTTP routes.
pub fn setup_code(root: &Path) -> Result<vesta_control::identity::management::PairCode> {
    let receipt = engine::read_receipt(root)?;
    if receipt.phase == "activated" {
        return Err("setup_already_activated_use_app".into());
    }
    let state = vesta_state::StateDirectory::new(root.join("private/state"), None)
        .map_err(|_| "setup_state_invalid")?;
    let db = vesta_state::StateDatabase::open(state, Arc::new(vesta_state::SystemClock))
        .map_err(|_| "setup_state_unavailable")?;
    let setup = vesta_control::onboarding::FirstServerSetup::reserve_local(
        db.clone(),
        &receipt.installation_id,
    )
    .map_err(|_| "setup_identity_unavailable")?;
    let result = setup
        .management()
        .issue_local(
            vesta_control::identity::BuiltinRole::Owner,
            vesta_control::identity::management::DEFAULT_PAIR_MINUTES,
        )
        .map_err(|_| "setup_code_unavailable".to_owned());
    drop(setup);
    db.close().map_err(|_| "setup_state_busy")?;
    result
}
pub fn ready(receipt: &Receipt) -> bool {
    check_ready(receipt).unwrap_or(false)
}
fn check_ready(receipt: &Receipt) -> Result<bool> {
    let body = pinned_local_json(
        receipt,
        SocketAddr::from((Ipv4Addr::LOCALHOST, receipt.answers.port)),
        "/api/v3/setup",
    )?;
    Ok(body["installation_id"] == receipt.installation_id
        && body["status"] == "awaiting_app_onboarding")
}
pub(crate) fn pinned_local_json(
    receipt: &Receipt,
    address: SocketAddr,
    path: &str,
) -> Result<serde_json::Value> {
    if !address.ip().is_loopback()
        || !matches!(
            path,
            "/api/v3/setup" | "/api/v3/update-health" | "/api/v3/status"
        )
    {
        return Err("setup_probe_target_refused".into());
    }
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
    let addr = address;
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
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
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
        return Err("setup_probe_status_invalid".into());
    }
    let body: serde_json::Value =
        serde_json::from_slice(&bytes[delimiter + 4..]).map_err(|_| "setup_response_invalid")?;
    Ok(body)
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
fn run_agent(root: &Path, lease: &HostLease) -> Result<()> {
    if crate::activation::run_registered(root, lease)?.is_some() {
        return Ok(());
    }
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

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn generated_documents_are_idempotent_and_never_replace_foreign_configuration() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("private")).unwrap();
        publish_document(root.path(), "private/launch.toml", b"owned-launch").unwrap();
        publish_document(root.path(), "private/launch.toml", b"owned-launch").unwrap();
        assert!(publish_document(root.path(), "private/launch.toml", b"different-launch").is_err());
        std::fs::write(root.path().join("host.toml"), b"operator-config").unwrap();
        assert!(publish_document(root.path(), "host.toml", b"generated-config").is_err());
        assert_eq!(
            std::fs::read(root.path().join("host.toml")).unwrap(),
            b"operator-config"
        );
        assert!(publish_document(root.path(), "../outside", b"refused").is_err());
        assert_eq!(
            std::fs::read_dir(root.path().join("private"))
                .unwrap()
                .count(),
            1
        );
    }
}
