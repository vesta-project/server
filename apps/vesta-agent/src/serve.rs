//! Agent composition only; CLI lifecycle clients never open a runtime themselves.
use crate::{control, local_files};
use std::{path::Path, process::ExitCode, sync::Arc, time::Duration};
use vesta_control::{
    config::{HostConfiguration, ListenerDescription, Supervision},
    host::{Host, HostShutdown},
};
use vesta_state::SystemClock;
use vesta_transport::{Transport, TransportShutdown};
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
pub async fn run(source: &Path, base: &Path, local_only: bool, allow_local: bool) -> ExitCode {
    let configuration = match HostConfiguration::load_file(source, base) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration_failed: {}", e.code());
            return ExitCode::FAILURE;
        }
    };
    let listener = match configuration.listener().clone() {
        ListenerDescription::Tls(listener) if !local_only && listener.bind().is_loopback() => {
            Some(listener)
        }
        ListenerDescription::Disabled if local_only || allow_local => None,
        ListenerDescription::Disabled => {
            eprintln!("listener_disabled");
            return ExitCode::FAILURE;
        }
        _ if local_only => None,
        _ => {
            eprintln!("local_public_listener_refused");
            return ExitCode::FAILURE;
        }
    };
    let source = match local_files::absolute(source, base) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(code) = crate::uninstall::check_start(&source) {
        eprintln!("{code}");
        return ExitCode::FAILURE;
    }
    let registry = source.parent().and_then(|directory| {
        crate::servers::managed_lease(directory)
            .map(|_| directory.parent().unwrap().parent().unwrap().to_owned())
    });
    let _registry_admission = match registry
        .as_ref()
        .map(|root| local_files::lock(&root.join("registry.lock")))
        .transpose()
    {
        Ok(guard) => guard,
        Err(code) => {
            eprintln!("{code}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(code) = crate::uninstall::check_start(&source) {
        eprintln!("{code}");
        return ExitCode::FAILURE;
    }
    // Excludes deletion across opening, all owned children, and actual shutdown.
    let lease_paths = [source.parent(), configuration.game().parent()]
        .into_iter()
        .flatten()
        .filter_map(crate::servers::managed_lease)
        .collect::<std::collections::BTreeSet<_>>();
    let _leases = match lease_paths
        .iter()
        .map(|p| local_files::lock(p))
        .collect::<local_files::Result<Vec<_>>>()
    {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let control_name = match crate::servers::control_name(&source) {
        Ok(name) => name,
        Err(code) => {
            eprintln!("{code}");
            return ExitCode::FAILURE;
        }
    };
    drop(_registry_admission);
    let installation_consumer =
        match vesta_control::onboarding::consumer::Consumer::from_configuration(&configuration) {
            Ok(c) => c,
            Err(code) => {
                eprintln!("{code}");
                return ExitCode::FAILURE;
            }
        };
    // The installation lease covers direct CLI starts as well as setup's child.
    // Reuse the existing session/orphan journal; a vanished lock is not stopped proof.
    let _installation_lease = match installation_consumer
        .as_ref()
        .map(|c| c.acquire_lease())
        .transpose()
    {
        Ok(l) => l,
        Err(code) => {
            eprintln!("{code}");
            return ExitCode::FAILURE;
        }
    };
    let directories = [source.parent(), configuration.game().parent()]
        .into_iter()
        .flatten()
        .filter(|p| crate::servers::managed_lease(p).is_some())
        .map(Path::to_owned)
        .chain(installation_consumer.as_ref().map(|c| c.root().to_owned()));
    let supervised = configuration.supervision() == Supervision::Separate;
    let sessions = match crate::sessions::Sessions::begin(directories, supervised) {
        Ok(sessions) => sessions,
        Err(code) => {
            eprintln!("{code}");
            return ExitCode::FAILURE;
        }
    };
    let (supervisor, supervised_state) = match configuration.supervision() {
        Supervision::InProcess => (None, crate::supervision::Release::none()),
        Supervision::Separate => match crate::supervision::attach(&configuration, &source, base) {
            Ok(attached) => (
                Some(attached.remote),
                crate::supervision::Release::new(attached.state),
            ),
            Err(code) => {
                eprintln!("{code}");
                // No runtime was opened; the supervisor (if any) owns its own game.
                if sessions.stopped().is_err() {
                    eprintln!("local_shutdown_marker_failed");
                }
                return ExitCode::FAILURE;
            }
        },
    };
    let host = match Host::open_with(configuration, Arc::new(SystemClock), supervisor) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("host_open_failed: {}", e.code());
            // Host::open has no fallible steps after starting its runtime; a
            // refusal has not spawned owned work and may close these markers.
            if sessions.stopped().is_err() {
                eprintln!("local_shutdown_marker_failed");
            }
            return ExitCode::FAILURE;
        }
    };
    let sftp = host.network_handle().sftp().clone();
    if !local_only && sftp.start().await.is_err() {
        eprintln!("sftp_start_failed");
    }
    let mut transport = if let Some(listener) = listener {
        let network = host.network_handle();
        match tokio::task::spawn_blocking(move || Transport::start(network, &listener)).await {
            Ok(Ok(t)) => Some(t),
            failed => {
                let code = match failed {
                    Ok(Err(error)) => error.code(),
                    _ => "transport_unavailable",
                };
                eprintln!("listener_failed: {code}");
                sftp.shutdown().await;
                shutdown_host(host, sessions);
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let control = match control::Server::start(
        host.configuration().state(),
        control_name,
        host.configuration().game().to_owned(),
        host.runtime().local_handle(),
        host.management().clone(),
        transport.as_ref().map(Transport::fingerprint_handle),
        match host.configuration().listener() {
            vesta_control::config::ListenerDescription::Tls(listener) => {
                listener.public_endpoint().map(str::to_owned)
            }
            _ => None,
        },
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            sftp.shutdown().await;
            shutdown_transport(transport.take());
            shutdown_host(host, sessions);
            return ExitCode::FAILURE;
        }
    };
    if let Some(nonce) = std::env::var_os("VESTA_UPDATE_READINESS_NONCE") {
        let ready = (|| {
            let consumer = installation_consumer
                .as_ref()
                .ok_or("update_consumer_not_registered")?;
            let nonce = nonce.to_str().ok_or("update_readiness_invalid")?;
            let manifest = std::env::var("VESTA_UPDATE_MANIFEST_SHA256")
                .map_err(|_| "update_readiness_invalid")?;
            let pin = transport
                .as_ref()
                .ok_or("update_readiness_tls_unproven")?
                .tls_fingerprint()
                .map_err(|_| "update_readiness_tls_unproven")?;
            consumer.publish_readiness(
                host.configuration(),
                host.database(),
                &pin,
                nonce,
                &manifest,
            )
        })();
        if let Err(code) = ready {
            eprintln!("{code}");
            let _ = control.shutdown().await;
            sftp.shutdown().await;
            shutdown_transport(transport.take());
            shutdown_host(host, sessions);
            return ExitCode::FAILURE;
        }
    }
    if let Some(t) = &transport {
        println!("serving; bound_address={}", t.local_addr());
    } else {
        println!("serving; local_control_only; remote_management_refused");
    }
    let mut shutdown = control.shutdown_signal();
    let already_requested = *shutdown.borrow();
    let signal_failed = if already_requested {
        false
    } else {
        tokio::select! {
            result = wait_for_ctrl_c(transport.as_ref()) => result.is_err(),
            _ = shutdown.changed() => false,
        }
    };
    if signal_failed {
        eprintln!("signal_wait_failed");
    }
    let control_failed = control.shutdown().await.is_err();
    if control_failed {
        eprintln!("local_control_shutdown_failed");
    }
    sftp.shutdown().await;
    shutdown_transport(transport);
    let result = shutdown_host(host, sessions);
    supervised_state.finish();
    if signal_failed || control_failed {
        ExitCode::FAILURE
    } else {
        result
    }
}
async fn wait_for_ctrl_c(transport: Option<&Transport>) -> Result<(), ()> {
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);
    let mut poll = tokio::time::interval(Duration::from_secs(5));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    poll.tick().await;
    loop {
        tokio::select! {
            result = &mut signal => return result.map_err(|_| ()),
            _ = poll.tick() => if let Some(t) = transport {
                match t.reload_tls() { Ok(true) => println!("tls_material_reloaded"), Ok(false) => {}, Err(e) => eprintln!("tls_reload_failed: {}", e.code()) }
            }
        }
    }
}
fn shutdown_transport(transport: Option<Transport>) {
    let Some(mut transport) = transport else {
        return;
    };
    loop {
        match transport.shutdown(SHUTDOWN_TIMEOUT) {
            Ok(TransportShutdown::Complete) => return,
            Ok(TransportShutdown::TimedOut(owned)) => {
                eprintln!("transport_shutdown_timed_out; retaining_runtime");
                transport = owned;
            }
            Err(_) => {
                eprintln!("transport_shutdown_failed");
                return;
            }
        }
    }
}
fn shutdown_host(mut host: Host, sessions: crate::sessions::Sessions) -> ExitCode {
    loop {
        match host.shutdown(SHUTDOWN_TIMEOUT) {
            Ok(HostShutdown::Complete) => {
                return match sessions.stopped() {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(_) => {
                        eprintln!("local_shutdown_marker_failed");
                        ExitCode::FAILURE
                    }
                };
            }
            Ok(HostShutdown::TimedOut(owned)) => {
                eprintln!("host_shutdown_timed_out; retaining_runtime");
                host = *owned;
            }
            Err(e) => {
                eprintln!("host_shutdown_failed: {}", e.code());
                return ExitCode::FAILURE;
            }
        }
    }
}
