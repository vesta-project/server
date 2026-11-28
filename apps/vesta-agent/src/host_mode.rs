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
    let mut signals = crate::shutdown_signal::ShutdownSignal::register()?;
    let watchdog = crate::shutdown::ExitWatchdog::arm(signals.observer())?;
    let source = std::path::absolute(source).map_err(|_| "host_config_invalid")?;
    let base = source.parent().ok_or("host_config_invalid")?;
    let bytes = std::fs::read(&source).map_err(|_| "host_config_unreadable")?;
    if bytes.len() > 64 * 1024 {
        return Err("host_config_invalid");
    }
    let mut config: Config =
        toml::from_str(std::str::from_utf8(&bytes).map_err(|_| "host_config_invalid")?)
            .map_err(|_| "host_config_invalid")?;
    let configured_roots = config.import_roots.clone();
    // Root-owned catalogue maintained by the local privileged CLI.
    let roots_file = base.join("import-roots.json");
    if roots_file.exists() {
        let roots: Vec<PathBuf> = serde_json::from_slice(
            &std::fs::read(&roots_file).map_err(|_| "host_import_roots_unreadable")?,
        )
        .map_err(|_| "host_import_roots_invalid")?;
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
    if let Some(p) = &config.worker_config
        && let Some(parent) = path(p).parent()
    {
        protected.push(parent.to_owned());
    }
    let servers_root = path(&config.servers);
    let java_root = path(&config.java_cache);
    for dir in [&servers_root, &java_root] {
        std::fs::create_dir_all(dir).map_err(|_| "host_servers_unavailable")?;
        protected.push(dir.clone());
    }
    if let Some(parent) = servers_root.parent() {
        protected.extend([parent.join("removed"), parent.join("retained-backups")]);
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
    #[cfg(target_os = "linux")]
    let folders = folders.with_prepared_root(
        base.join(vesta_control::machine::imports::prepare::PREPARED_DIRECTORY),
    );
    let folders = Arc::new(folders);
    let root_base = base.to_owned();
    folders.set_root_loader(move || {
        let mut paths = configured_roots.clone();
        match std::fs::read(&roots_file) {
            Ok(bytes) if bytes.len() <= 64 * 1024 => paths.extend(
                serde_json::from_slice::<Vec<PathBuf>>(&bytes)
                    .map_err(|_| vesta_control::machine::imports::ImportError::Unavailable)?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(vesta_control::machine::imports::ImportError::Unavailable),
        }
        Ok(paths
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let full = if p.is_absolute() {
                    p
                } else {
                    root_base.join(p)
                };
                ImportRootConfig {
                    root_id: format!("root-{i}"),
                    label: full
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| full.display().to_string()),
                    path: full,
                }
            })
            .collect())
    });
    let jobs = ImportJobs::open(
        state.clone(),
        folders.clone(),
        &servers_root,
        Arc::new(HostEnvironment {
            java_cache: java_root.clone(),
        }),
    )
    .map_err(|_| "host_imports_unavailable")?;
    let factory = (jobs.clone(), clock.clone());
    let mut router = HostRouter::new(identity.clone(), config.name.clone(), folders)
        .with_startup_worker_factory(jobs.clone(), move |id, boot| {
            factory.0.open_worker(id, factory.1.clone(), boot)
        })
        .with_storage_java_cache(java_root.clone())
        .with_create_installer(Arc::new(
            vesta_control::machine::creates::LiveCreateInstaller::new(
                java_root,
                jobs.servers_root().to_owned(),
            ),
        ));
    let entries = state
        .snapshot()
        .map_err(|_| "host_state_unavailable")?
        .primary_account_id
        .map(|a| state.visible_servers(a))
        .transpose()
        .map_err(|_| "host_registry_unavailable")?
        .unwrap_or_default();
    // Managed and explicitly configured servers share the Host worker registry,
    // including the enablement and shutdown paths.
    let imported = |id| jobs.server_dir(id).join("host.toml").is_file();
    let supervisor_release = Arc::new(std::sync::Mutex::new(None));
    if let Some((entry, _)) = entries.iter().find(|(entry, _)| !imported(entry.server_id)) {
        let worker_source = config.worker_config.as_ref().map(|p| path(p));
        let worker_base = base.to_path_buf();
        let clock = clock.clone();
        let state = state.clone();
        let release = supervisor_release.clone();
        router = router.with_configured_worker(entry.server_id, move |id, boot| {
            use vesta_control::machine::{imports::jobs::JobError, startup::StartupMode};
            let worker_source = worker_source
                .as_ref()
                .ok_or(JobError("host_worker_config_required"))?;
            let config = HostConfiguration::load_file(worker_source, &worker_base)
                .map_err(|_| JobError("host_worker_config_invalid"))?;
            let remote = if config.supervision() == Supervision::Separate {
                let mut release = release
                    .lock()
                    .map_err(|_| JobError("host_supervisor_unavailable"))?;
                let attached = crate::supervision::attach(
                    &config,
                    worker_source,
                    worker_source.parent().unwrap_or(&worker_base),
                )
                .map_err(|_| JobError("host_supervisor_unavailable"))?;
                if release.is_none() {
                    *release = Some(crate::supervision::Release::new(attached.state));
                }
                Some(attached.remote)
            } else {
                None
            };
            let startup = if boot {
                state
                    .startup_settings(id)?
                    .map_or(StartupMode::Configured, StartupMode::Managed)
            } else {
                StartupMode::Deferred
            };
            Worker::open_with_policy(
                config,
                clock.clone(),
                remote,
                state.worker_policy(id)?,
                startup,
            )
            .map_err(|_| JobError("host_worker_unavailable"))
        });
    }
    if let Some(jobs) = router.imports() {
        vesta_control::machine::removed::reconcile(&identity.state, jobs.servers_root())
            .map_err(|_| "host_removal_recovery_failed")?;
    }
    for (id, e) in router.restore_registered_workers() {
        eprintln!("Vesta Server could not open server {id}: {e}");
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
    let mut advertised_name = router.display_name();
    let mut discovery = if config.local_only || !config.discovery_interfaces.is_empty() {
        Some(
            DiscoveryAdvertiser::start(DiscoveryConfig {
                txt: TxtRecord {
                    id: snap.host_id.to_string(),
                    fingerprint: transport
                        .tls_fingerprint()
                        .map_err(|_| "host_tls_unavailable")?,
                    name: advertised_name.clone(),
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
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {request=signals.wait()=>{watchdog.begin(request?);break},_=tick.tick()=>{
            // Claim and owner renames are re-announced on the next tick.
            let next=claim_state();
            let name=router.display_name();
            if next!=advertised || name!=advertised_name {
                if let Some(ad)=&mut discovery{let _=ad.update(next,name.clone());}
                advertised=next;
                advertised_name=name;
            }
        }}
    }
    eprintln!("host_shutdown_started");
    // Quiesce all owners before any listener/job wait; a stuck job on one
    // server must not postpone the shutdown signal to every other server.
    router.request_system_shutdown();
    if let Some(ad) = discovery {
        let _ = ad.shutdown();
    }
    let local_shutdown = local.shutdown();
    tokio::pin!(local_shutdown);
    let mut progress = tokio::time::interval(Duration::from_millis(10));
    let mut clean = loop {
        tokio::select! {
            result=&mut local_shutdown=>break result.is_ok(),
            _=progress.tick()=>{router.advance_system_shutdown();}
        }
    };
    let mut transport = Some(transport);
    while let Some(current) = transport.take() {
        router.advance_system_shutdown();
        match current.shutdown(watchdog.remaining().min(Duration::from_millis(25))) {
            Ok(TransportShutdown::Complete) => (),
            Ok(TransportShutdown::TimedOut(owner)) => transport = Some(owner),
            Err(_) => {
                clean = false;
            }
        }
        if transport.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    // Async requests/cache writers and import jobs retain state access until
    // transport/job drain. Only then can a worker close its database.
    while !router.shutdown_jobs_complete() {
        router.advance_system_shutdown();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut owners: std::collections::VecDeque<_> =
        router.take_owned_workers().into_iter().collect();
    while !owners.is_empty() {
        let count = owners.len();
        for _ in 0..count {
            let current = owners.pop_front().expect("owned worker");
            match current.shutdown_system(Duration::ZERO) {
                Ok(WorkerShutdown::Complete) => (),
                Ok(WorkerShutdown::TimedOut(owner)) => owners.push_back(*owner),
                Err(error) => {
                    eprintln!("host_worker_shutdown_failed: {}", error.code());
                    clean = false;
                }
            }
        }
        if !owners.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    if let Some(release) = supervisor_release
        .lock()
        .expect("supervisor release")
        .take()
    {
        release.finish();
    }
    signals.complete();
    watchdog.finish();
    if clean {
        eprintln!("host_shutdown_complete");
        Ok(())
    } else {
        Err("host_shutdown_incomplete")
    }
}
