//! Trusted-local host entrypoint for configuration inspection and serving.
use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};
use vesta_control::{
    config::{HostConfiguration, ListenerDescription},
    host::{Host, HostShutdown},
};
use vesta_state::SystemClock;
use vesta_transport::{Transport, TransportShutdown};

const USAGE: &str = "Usage: vesta-agent check-config <path>";
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    args.next();
    let Some(command) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if (command == "--help" || command == "-h") && args.next().is_none() {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if command != "check-config" && command != "serve" {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let Some(source) = args.next().map(PathBuf::from) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let Ok(base) = std::env::current_dir() else {
        eprintln!("configuration_failed: config_environment_unavailable");
        return ExitCode::FAILURE;
    };
    if command == "check-config" {
        return check_config(&source, &base);
    }
    serve(&source, &base)
}

fn check_config(source: &std::path::Path, base: &std::path::Path) -> ExitCode {
    match HostConfiguration::load_file(source, base)
        .and_then(|configuration| configuration.validate_host_paths())
    {
        Ok(_) => {
            println!("configuration_valid; runtime_and_transport_not_started");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("configuration_failed: {}", error.code());
            ExitCode::FAILURE
        }
    }
}

fn serve(source: &std::path::Path, base: &std::path::Path) -> ExitCode {
    let configuration = match HostConfiguration::load_file(source, base) {
        Ok(configuration) => configuration,
        Err(error) => {
            eprintln!("configuration_failed: {}", error.code());
            return ExitCode::FAILURE;
        }
    };
    let listener = match configuration.listener().clone() {
        ListenerDescription::Tls(listener) => listener,
        ListenerDescription::Disabled => {
            eprintln!("listener_disabled");
            return ExitCode::FAILURE;
        }
    };
    let host = match Host::open(configuration, Arc::new(SystemClock)) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("host_open_failed: {}", error.code());
            return ExitCode::FAILURE;
        }
    };
    let mut transport = match Transport::start(host.network_handle(), &listener) {
        Ok(transport) => transport,
        Err(_) => {
            eprintln!("transport_start_failed");
            shutdown_host(host);
            return ExitCode::FAILURE;
        }
    };
    println!("serving; bound_address={}", transport.local_addr());
    let signal_failed = wait_for_ctrl_c(&transport).is_err();
    if signal_failed {
        eprintln!("signal_wait_failed");
    }

    loop {
        match transport.shutdown(SHUTDOWN_TIMEOUT) {
            Ok(TransportShutdown::Complete) => break,
            Ok(TransportShutdown::TimedOut(owned)) => {
                eprintln!("transport_shutdown_timed_out; retaining_runtime");
                transport = owned;
            }
            Err(_) => {
                eprintln!("transport_shutdown_failed");
                shutdown_host(host);
                return ExitCode::FAILURE;
            }
        }
    }
    let shutdown = shutdown_host(host);
    if signal_failed {
        ExitCode::FAILURE
    } else {
        shutdown
    }
}

fn wait_for_ctrl_c(transport: &Transport) -> Result<(), ()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ())?;
    runtime.block_on(async {
        let signal = tokio::signal::ctrl_c();
        tokio::pin!(signal);
        // One bounded synchronous read/validation at a time; no watcher queue or
        // unbounded background jobs. Metadata plus content digest detect changes.
        let mut poll = tokio::time::interval(Duration::from_secs(5));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        poll.tick().await;
        loop {
            tokio::select! {
                result = &mut signal => return result.map_err(|_| ()),
                _ = poll.tick() => {
                    match transport.reload_tls() {
                        Ok(true) => println!("tls_material_reloaded"),
                        Ok(false) => {},
                        Err(error) => eprintln!("tls_reload_failed: {}", error.code()),
                    }
                }
            }
        }
    })
}

fn shutdown_host(mut host: Host) -> ExitCode {
    loop {
        match host.shutdown(SHUTDOWN_TIMEOUT) {
            Ok(HostShutdown::Complete) => return ExitCode::SUCCESS,
            Ok(HostShutdown::TimedOut(owned)) => {
                eprintln!("host_shutdown_timed_out; retaining_runtime");
                host = *owned;
            }
            Err(error) => {
                eprintln!("host_shutdown_failed: {}", error.code());
                return ExitCode::FAILURE;
            }
        }
    }
}
