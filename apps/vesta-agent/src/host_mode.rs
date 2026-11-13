//! Persistent machine composition. Config paths are relative to host.toml.
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};
use vesta_control::{
    config::{HostConfiguration, Supervision, TlsListener},
    machine::{
        HostState,
        imports::{
            FolderBrowser, ImportRootConfig,
            jobs::{HostEnvironment, ImportJobs},
        },
    },
    worker::{Worker, WorkerShutdown},
};
use vesta_transport::{
    Transport, TransportShutdown,
    discovery::{ClaimState, DiscoveryAdvertiser, DiscoveryConfig, TxtRecord},
    host_identity::HostIdentity,
    host_router::HostRouter,
};
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    state: PathBuf,
    certificate: PathBuf,
    private_key: PathBuf,
    #[serde(default = "bind")]
    bind: std::net::IpAddr,
    #[serde(default = "port")]
    port: u16,
    #[serde(default = "name")]
    name: String,
    #[serde(default)]
    local_only: bool,
    #[serde(default)]
    worker_config: Option<PathBuf>,
    #[serde(default)]
    import_roots: Vec<PathBuf>,
    /// Managed storage for imported servers (`<servers>/<serverId>/game`).
    #[serde(default = "servers")]
    servers: PathBuf,
    /// Shared checksum-verified managed Java runtimes.
    #[serde(default = "java_cache")]
    java_cache: PathBuf,
    #[serde(default)]
    discovery_interfaces: Vec<String>,
    #[serde(default = "hostname")]
    discovery_hostname: String,
}
fn bind() -> std::net::IpAddr {
    "0.0.0.0".parse().unwrap()
}
fn port() -> u16 {
    25500
}
fn name() -> String {
    "Vesta Server".into()
}
fn hostname() -> String {
    "vesta.local.".into()
}
fn servers() -> PathBuf {
    "servers".into()
}
fn java_cache() -> PathBuf {
    "java".into()
}
pub async fn run(source: &Path) -> ExitCode {
    match run_inner(source).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}
async fn run_inner(source: &Path) -> Result<(), &'static str> {
    let source = std::path::absolute(source).map_err(|_| "host_config_invalid")?;
    let base = source.parent().ok_or("host_config_invalid")?;
    let bytes = std::fs::read(&source).map_err(|_| "host_config_unreadable")?;
    if bytes.len() > 64 * 1024 {
        return Err("host_config_invalid");
    }
    let mut config: Config =
        toml::from_str(std::str::from_utf8(&bytes).map_err(|_| "host_config_invalid")?)
            .map_err(|_| "host_config_invalid")?;
    // Root-owned catalogue maintained by the local privileged CLI.
    let roots_file = base.join("import-roots.json");
    if roots_file.exists() {
        let roots: Vec<PathBuf> = serde_json::from_slice(&std::fs::read(roots_file).map_err(|_| "host_import_roots_unreadable")?).map_err(|_| "host_import_roots_invalid")?;
        config.import_roots.extend(roots);
    }
    let path = |p: &Path| {
        if p.is_absolute() {
            p.to_owned()
        } else {
            base.join(p)
        }
    };
    let state_path = path(&config.state);
    let clock = Arc::new(vesta_state::SystemClock);
    let state =
        HostState::open(&state_path, clock.clone()).map_err(|_| "host_state_unavailable")?;
    let identity =
        Arc::new(HostIdentity::new(state.clone()).map_err(|_| "host_identity_unavailable")?);
    let worker_configuration = config
        .worker_config
        .as_ref()
        .map(|p| {
            let source = path(p);
            HostConfiguration::load_file(&source, source.parent().unwrap_or(base))
        })
        .transpose()
        .map_err(|_| "host_worker_config_invalid")?;
    // The data directory holding host.toml is never browsable or importable.
    let mut protected = vec![
        base.to_owned(),
        state_path.clone(),
        source.clone(),
        path(&config.certificate),
        path(&config.private_key),
    ];
    #[cfg(target_os = "linux")]
    if let Some(install) = Path::new(vesta_setup::linux_install::DATA_ROOT_RECORD).parent() {
        protected.push(install.to_owned());
    }
    if let Some(c) = &worker_configuration {
        protected.extend([
            c.game().to_owned(),
            c.state().to_owned(),
            c.launch_definition().to_owned(),
        ]);
    }
    if let Some(p) = &config.worker_config {
        if let Some(parent) = path(p).parent() {
            protected.push(parent.to_owned());
        }
    }
    let servers_root = path(&config.servers);
    let java_root = path(&config.java_cache);
    for dir in [&servers_root, &java_root] {
        std::fs::create_dir_all(dir).map_err(|_| "host_servers_unavailable")?;
        protected.push(dir.clone());
    }
    let (folders, skipped) = FolderBrowser::new_skipping_unusable(
        config
            .import_roots
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let full = path(p);
                ImportRootConfig {
                    root_id: format!("root-{i}"),
                    label: full
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| full.display().to_string()),
                    path: full,
                }
            })
            .collect(),
        protected,
    )
    .map_err(|_| "host_import_roots_invalid")?;
    for (root, error) in skipped {
        eprintln!(
            "Vesta Server skipped saved import folder {} ({error})",
            root.path.display()
        );
    }
    let folders = Arc::new(folders);
    let jobs = ImportJobs::open(
        state.clone(),
        folders.clone(),
        &servers_root,
        Arc::new(HostEnvironment {
            java_cache: java_root,
        }),
    )
    .map_err(|_| "host_imports_unavailable")?;
    let factory = (jobs.clone(), clock.clone());
    let router = HostRouter::new(identity.clone(), config.name.clone(), folders).with_imports(
        jobs.clone(),
        move |id| factory.0.create_worker(id, factory.1.clone()),
    );
    let entries = state
        .snapshot()
        .map_err(|_| "host_state_unavailable")?
        .primary_account_id
        .map(|a| state.visible_servers(a))
        .transpose()
        .map_err(|_| "host_registry_unavailable")?
        .unwrap_or_default();
    // Imported servers come back from their managed host.toml after a restart;
    // a failure is reported and leaves the host (and other servers) running.
    let imported = |id| jobs.server_dir(id).join("host.toml").is_file();
    for (entry, _) in entries.iter().filter(|(e, _)| imported(e.server_id)) {
        match jobs.create_worker(entry.server_id, clock.clone()) {
            Ok(w) => {
                router.adopt_worker(w);
            }
            Err(e) => eprintln!("Vesta Server could not open server {}: {e}", entry.server_id),
        }
    }
    let mut worker = None;
    let mut supervisor_release = None;
    if let Some((entry, _)) = entries.iter().find(|(e, _)| !imported(e.server_id)) {
        let worker_source = config
            .worker_config
            .as_ref()
            .map(|p| path(p))
            .ok_or("host_worker_config_required")?;
        let c = HostConfiguration::load_file(&worker_source, base)
            .map_err(|_| "host_worker_config_invalid")?;
        let remote = if c.supervision() == Supervision::Separate {
            let attached = crate::supervision::attach(
                &c,
                &worker_source,
                worker_source.parent().unwrap_or(base),
            )
            .map_err(|_| "host_supervisor_unavailable")?;
            supervisor_release = Some(crate::supervision::Release::new(attached.state));
            Some(attached.remote)
        } else {
            None
        };
        let w = Worker::from_registry(
            Some(entry),
            c,
            clock,
            remote,
            state
                .snapshot()
                .map_err(|_| "host_state_unavailable")?
                .host_id,
        )
        .map_err(|_| "host_worker_unavailable")?
        .ok_or("host_worker_unavailable")?;
        router.add_worker(Arc::new(w.handle()));
        worker = Some(w);
    }
    let bind = if config.local_only {
        "127.0.0.1".parse().unwrap()
    } else {
        config.bind
    };
    let listener = TlsListener::host(
        bind,
        config.port,
        path(&config.certificate),
        path(&config.private_key),
    )
    .map_err(|_| "host_listener_invalid")?;
    let transport = Transport::start_host(router.clone(), &listener)
        .map_err(|_| "host_listener_unavailable")?;
    let local = crate::host_local::Server::start(&state_path, identity.clone())
        .await
        .map_err(|_| "host_local_unavailable")?;
    let snap = state.snapshot().map_err(|_| "host_state_unavailable")?;
    println!("Vesta Server is running on {}", transport.local_addr());
    let claim_state = || {
        if state
            .snapshot()
            .is_ok_and(|s| s.primary_account_id.is_some())
        {
            ClaimState::Claimed
        } else {
            ClaimState::Unclaimed
        }
    };
    let mut advertised = claim_state();
    let mut discovery = if config.local_only || !config.discovery_interfaces.is_empty() {
        Some(
            DiscoveryAdvertiser::start(DiscoveryConfig {
                txt: TxtRecord {
                    id: snap.host_id.to_string(),
                    fingerprint: transport
                        .tls_fingerprint()
                        .map_err(|_| "host_tls_unavailable")?,
                    name: config.name.clone(),
                    state: advertised,
                },
                hostname: config.discovery_hostname,
                port: transport.local_addr().port(),
                interfaces: config.discovery_interfaces,
                local_only: config.local_only,
            })
            .map_err(|_| "host_discovery_unavailable")?,
        )
    } else {
        eprintln!("Vesta Server LAN discovery needs discovery-interfaces in host.toml");
        None
    };
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {_=&mut signal=>break,_=tick.tick()=>{let next=claim_state();if next!=advertised {if let Some(ad)=&mut discovery{let _=ad.update(next,config.name.clone());}advertised=next;}}}
    }
    if let Some(ad) = discovery {
        let _ = ad.shutdown();
    }
    local
        .shutdown()
        .await
        .map_err(|_| "host_local_shutdown_failed")?;
    let mut t = transport;
    loop {
        match t
            .shutdown(Duration::from_secs(5))
            .map_err(|_| "host_shutdown_failed")?
        {
            TransportShutdown::Complete => break,
            TransportShutdown::TimedOut(remaining) => t = remaining,
        }
    }
    let owned = router.take_owned_workers();
    for mut w in worker.into_iter().chain(owned) {
        loop {
            match w
                .shutdown(Duration::from_secs(5))
                .map_err(|_| "host_worker_shutdown_failed")?
            {
                WorkerShutdown::Complete => break,
                WorkerShutdown::TimedOut(remaining) => w = *remaining,
            }
        }
    }
    if let Some(release) = supervisor_release {
        release.finish();
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
