//! Native installation boundary for signed update trials. Restricted probes
//! never open a runtime, migrate SQLite, accept EULA or launch Minecraft.
use crate::{engine, host};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::SocketAddr,
    path::Path,
    process::{Child, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use vesta_control::{
    config::HostConfiguration,
    onboarding::consumer::{Consumer, StoppedConsumer},
};
use vesta_update::{
    VerifiedPayload,
    activation::{ACTIVATION_PROTOCOL, ActivationHost, Health, Scope},
};
type Result<T> = std::result::Result<T, String>;

pub(crate) struct NativeHost<'a> {
    root: std::path::PathBuf,
    stopped: StoppedConsumer,
    _host: &'a host::HostLease,
}
impl<'a> NativeHost<'a> {
    pub(crate) fn open(root: &Path, lease: &'a host::HostLease) -> Result<Self> {
        if crate::paths::comparable(root) != crate::paths::comparable(&lease.root) {
            return Err("update_host_lease_mismatch".into());
        }
        let receipt = engine::read_receipt(root)?;
        if receipt.phase != "activated" {
            return Err("update_first_server_not_ready".into());
        }
        let config = HostConfiguration::load_file(&root.join("host.toml"), root)
            .map_err(|_| "update_host_configuration_invalid")?;
        let consumer = Consumer::from_configuration(&config)
            .map_err(str::to_owned)?
            .ok_or("update_consumer_not_registered")?;
        if consumer.record().installation_id != receipt.installation_id {
            return Err("update_consumer_binding_invalid".into());
        }
        let stopped = consumer.stopped().map_err(str::to_owned)?;
        Ok(Self {
            root: root.to_owned(),
            stopped,
            _host: lease,
        })
    }
}
fn scope(
    root: &Path,
    observation: vesta_control::onboarding::consumer::Observation,
) -> Result<Scope> {
    let receipt = engine::read_receipt(root)?;
    let tls_sha256 = host::identity_fingerprint(root)?;
    if tls_sha256 != receipt.tls_sha256 || observation.installation_id != receipt.installation_id {
        return Err("update_tls_identity_changed".into());
    }
    Ok(Scope {
        installation_id: observation.installation_id,
        server_id: observation.server_id,
        state_schema: observation.state_schema,
        supervisor_ipc: observation.supervisor_ipc,
        tls_sha256,
        configuration_sha256: observation.configuration_sha256,
        launch_sha256: observation.launch_sha256,
    })
}
impl ActivationHost for NativeHost<'_> {
    fn observe(&mut self) -> Result<Scope> {
        scope(&self.root, self.stopped.observe().map_err(str::to_owned)?)
    }
    fn trial(&mut self, payload: &mut VerifiedPayload) -> Result<Health> {
        verify_component_capabilities(payload)?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let name = if cfg!(windows) {
            "vesta-setup.exe"
        } else {
            "vesta-setup"
        };
        let mut command = payload.command(name)?;
        command
            .args(["update", "probe", "--root"])
            .arg(&self.root)
            .args([
                "--manifest-sha256",
                payload.manifest_sha256(),
                "--probe-nonce",
                &nonce,
            ])
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().map_err(|_| "update_probe_start_failed")?;
        let stdout = child.stdout.take().ok_or("update_probe_pipe_failed")?;
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut line = Vec::new();
            let result = BufReader::new(stdout)
                .take(8193)
                .read_until(b'\n', &mut line)
                .map(|_| line);
            let _ = tx.send(result);
        });
        let result = (|| {
            let line = rx
                .recv_timeout(Duration::from_secs(20))
                .map_err(|_| "update_probe_start_timeout")?
                .map_err(|_| "update_probe_response_invalid")?;
            if line.len() > 8192 {
                return Err("update_probe_response_invalid".into());
            }
            let hello: serde_json::Value =
                serde_json::from_slice(&line).map_err(|_| "update_probe_response_invalid")?;
            if hello["nonce"] != nonce || hello["protocol"] != ACTIVATION_PROTOCOL {
                return Err("update_probe_response_invalid".into());
            }
            let address: SocketAddr = hello["address"]
                .as_str()
                .ok_or("update_probe_response_invalid")?
                .parse()
                .map_err(|_| "update_probe_response_invalid")?;
            if !address.ip().is_loopback() || address.port() == 0 {
                return Err("update_probe_target_refused".into());
            }
            let receipt = engine::read_receipt(&self.root)?;
            let response = host::pinned_local_json(&receipt, address, "/api/v3/update-health")?;
            if response["nonce"] != nonce || response["protocol"] != ACTIVATION_PROTOCOL {
                return Err("update_probe_response_invalid".into());
            }
            let proof: Health = serde_json::from_value(response["health"].clone())
                .map_err(|_| "update_probe_response_invalid")?;
            if proof.scope != self.observe()? || proof.manifest_sha256 != payload.manifest_sha256()
            {
                return Err("update_probe_identity_mismatch".into());
            }
            Ok(proof)
        })();
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"stop\n");
        }
        let finish = finish_probe(&mut child);
        let _ = reader.join();
        result.and_then(|proof| {
            finish?;
            Ok(proof)
        })
    }
}
/// This terminates only the exact restricted helper spawned above, never a
/// runtime/agent/game PID. The timeout branch is not a Minecraft fault test.
fn finish_probe(child: &mut Child) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                return if exit.success() {
                    Ok(())
                } else {
                    Err("update_probe_failed".into())
                };
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("update_probe_stop_failed".into());
            }
        }
    }
}

fn now() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or("update_clock_invalid".into())
}
/// Stable host handoff; active bytes are never overwritten. Only the exact
/// child's protected readiness plus its real pinned endpoint commits selection.
pub(crate) fn run_registered(root: &Path, lease: &host::HostLease) -> Result<Option<()>> {
    if !root.join("private/update-state").exists() {
        return Ok(None);
    }
    let receipt = engine::read_receipt(root)?;
    let store = vesta_update::UpdateStore::open(&root.join("private/update-state"))?;
    if !store.registered_for(&receipt.installation_id)? {
        return Ok(None);
    }
    let mut native = NativeHost::open(root, lease)?;
    let expected = native.observe()?;
    let consumer = native.stopped.consumer().clone();
    let (intent, mut payload) = store.payload_for_start(&mut native, now()?)?;
    let config = HostConfiguration::load_file(&root.join("host.toml"), root)
        .map_err(|_| "update_host_configuration_invalid")?;
    let vesta_control::config::ListenerDescription::Tls(listener) = config.listener() else {
        return Err("update_readiness_tls_unproven".into());
    };
    let address = SocketAddr::from((listener.bind(), listener.port()));
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let manifest = payload.manifest_sha256().to_owned();
    let mut command = payload.command(if cfg!(windows) {
        "vesta-agent.exe"
    } else {
        "vesta-agent"
    })?;
    command
        .arg("serve")
        .arg(root.join("host.toml"))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("VESTA_UPDATE_READINESS_NONCE", &nonce)
        .env("VESTA_UPDATE_MANIFEST_SHA256", &manifest)
        .env("VESTA_INSTALLATION_ROOT", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    drop(native); // The child takes these same consumer/runtime leases.
    let mut child = command.spawn().map_err(|_| "update_agent_start_failed")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let startup = (|| {
        loop {
            if child
                .try_wait()
                .map_err(|_| "update_agent_wait_failed")?
                .is_some()
            {
                return Err("update_agent_failed_before_readiness".to_owned());
            }
            if let Ok(Some(ready)) = consumer.read_readiness(&nonce) {
                if ready.pid != child.id()
                    || ready.manifest_sha256 != manifest
                    || ready.tls_sha256 != expected.tls_sha256
                {
                    return Err("update_agent_readiness_mismatch".into());
                }
                let observed = scope(root, ready.observation)?;
                if observed != expected {
                    return Err("update_agent_readiness_mismatch".into());
                }
                let reply = host::pinned_local_json(&receipt, address, "/api/v3/status")?;
                if reply["server_id"] != expected.server_id || reply["api_version"] != "3" {
                    return Err("update_agent_endpoint_mismatch".into());
                }
                if child
                    .try_wait()
                    .map_err(|_| "update_agent_wait_failed")?
                    .is_some()
                {
                    return Err("update_agent_failed_before_readiness".into());
                }
                if let Some(id) = &intent {
                    store.confirm_started(
                        id,
                        &mut payload,
                        &Health {
                            manifest_sha256: manifest.clone(),
                            scope: observed,
                        },
                    )?;
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("update_agent_readiness_unproven".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    })();
    if let Err(error) = startup {
        // Never kill a service/game or select a fallback while stop is unknown.
        // A live child keeps its consumer lease and unclean-session witness.
        if child
            .try_wait()
            .map_err(|_| "update_agent_wait_failed")?
            .is_some()
        {
            if let Some(id) = &intent {
                let mut stopped = NativeHost::open(root, lease)?;
                store.recover_activation(id, &mut stopped, now()?)?;
                drop(stopped);
                drop(payload);
                return run_registered(root, lease); // Recovered intents are no longer selected.
            }
        }
        return Err(error);
    }
    let exit = child.wait().map_err(|_| "update_agent_wait_failed")?;
    if exit.success() {
        Ok(Some(()))
    } else {
        Err("update_agent_exited_after_readiness".into())
    }
}

/// Child command invoked only by the native signed-payload runner. Its ephemeral
/// loopback endpoint exposes fixed nonsecret identity observations and no writes.
pub(crate) fn probe(root: &Path, manifest: &str, nonce: &str) -> Result<()> {
    let hex = |s: &str, len| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex(manifest, 64) || !hex(nonce, 32) {
        return Err("update_probe_input_invalid".into());
    }
    let config = HostConfiguration::load_file(&root.join("host.toml"), root)
        .map_err(|_| "update_host_configuration_invalid")?;
    let consumer = Consumer::from_configuration(&config)
        .map_err(str::to_owned)?
        .ok_or("update_consumer_not_registered")?;
    let observed = consumer.observe_readonly().map_err(str::to_owned)?;
    let actual = scope(root, observed.clone())?;
    let data = vesta_transport::LocalUpdateProbe {
        protocol: ACTIVATION_PROTOCOL,
        nonce: nonce.into(),
        manifest_sha256: manifest.into(),
        tls_sha256: actual.tls_sha256.clone(),
        observation: observed,
    };
    let listener = vesta_control::config::TlsListener::pending_local(
        1,
        root.join("private/certificate.pem"),
        root.join("private/key.pem"),
    )
    .map_err(|_| "update_probe_tls_invalid")?
    .on_ephemeral_port();
    let transport =
        vesta_transport::Transport::start_local_probe(data, &listener, &actual.tls_sha256)
            .map_err(|_| "update_probe_tls_failed")?;
    println!(
        "{}",
        serde_json::json!({"protocol":ACTIVATION_PROTOCOL,"nonce":nonce,"address":transport.local_addr().to_string()})
    );
    std::io::stdout()
        .flush()
        .map_err(|_| "update_probe_output_failed")?;
    let (tx, rx) = mpsc::sync_channel(1);
    let _input = std::thread::spawn(move || {
        let mut input = Vec::new();
        let result = std::io::stdin().take(128).read_to_end(&mut input);
        let _ = tx.send(result);
    });
    let _ = rx.recv_timeout(Duration::from_secs(20));
    match transport
        .shutdown(Duration::from_secs(5))
        .map_err(|_| "update_probe_shutdown_failed")?
    {
        vesta_transport::TransportShutdown::Complete => Ok(()),
        vesta_transport::TransportShutdown::TimedOut(_) => {
            Err("update_probe_shutdown_incomplete".into())
        }
    }
}

/// The original installation remains an immutable bootstrap. Execute the selected
/// setup component before it starts the selected agent; never mix release components.
pub(crate) fn delegate_host(root: &Path) -> Result<Option<()>> {
    if !root.join("private/update-state").exists() {
        return Ok(None);
    }
    let receipt = engine::read_receipt(root)?;
    let store = vesta_update::UpdateStore::open(&root.join("private/update-state"))?;
    if !store.registered_for(&receipt.installation_id)? {
        return Ok(None);
    }
    let lease = host::acquire_host_lease(root)?;
    let mut native = NativeHost::open(root, &lease)?;
    let (intent, mut payload) = store.payload_for_start(&mut native, now()?)?;
    let name = if cfg!(windows) {
        "vesta-setup.exe"
    } else {
        "vesta-setup"
    };
    let current = std::fs::canonicalize(
        std::env::current_exe().map_err(|_| "update_executable_unavailable")?,
    )
    .map_err(|_| "update_executable_unavailable")?;
    let selected = std::fs::canonicalize(payload.directory().join("bin").join(name))
        .map_err(|_| "update_executable_unavailable")?;
    if crate::paths::comparable(&current) == crate::paths::comparable(&selected) {
        return Ok(None);
    }
    let mut command = payload.command(name)?;
    command
        .arg("host")
        .arg("--root")
        .arg(root)
        .current_dir(root)
        .env("VESTA_INSTALLATION_ROOT", root)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    drop(native);
    drop(lease);
    let exit = command.status().map_err(|_| "update_host_start_failed")?;
    if exit.success() {
        return Ok(Some(()));
    }
    if let Some(id) = intent {
        let status = store.activation_status(&id)?;
        if status.state == "restart_pending" {
            // Child exit alone is insufficient: acquire all stopped fences again.
            let lease = host::acquire_host_lease(root)?;
            let mut stopped = NativeHost::open(root, &lease)?;
            store.recover_activation(&id, &mut stopped, now()?)?;
            return Ok(None);
        }
    }
    Err("update_host_exited".into())
}

fn verify_component_capabilities(payload: &mut VerifiedPayload) -> Result<()> {
    let expected =
        vesta_state::update_capabilities(vesta_control::onboarding::compiled_supervisor_ipc());
    for stem in ["vesta-setup", "vesta-agent", "vesta-supervisor", "vesta"] {
        let name = if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.into()
        };
        let mut command = payload.command(&name)?;
        command
            .args(["update", "capabilities"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .map_err(|_| "update_component_capabilities_unavailable")?;
        let stdout = child.stdout.take().ok_or("update_probe_pipe_failed")?;
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(8193).read_to_end(&mut bytes).map(|_| bytes);
            let _ = tx.send(result);
        });
        let output = rx
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "update_component_capabilities_timeout");
        let finish = finish_probe(&mut child);
        let _ = reader.join();
        let output = output?.map_err(|_| "update_component_capabilities_invalid")?;
        finish?;
        if output.len() > 8192
            || serde_json::from_slice::<serde_json::Value>(&output)
                .ok()
                .as_ref()
                != Some(&expected)
        {
            return Err("update_component_capabilities_mismatch".into());
        }
    }
    Ok(())
}
