//! Per-server process supervisor entrypoint. Started detached by `vesta-agent serve`
//! when `[process] supervision = "separate"`; it has no network listener and launches
//! only the launch definition named by the validated host configuration.
use std::{path::PathBuf, process::ExitCode};
use vesta_control::{config::HostConfiguration, host::supervised_launch};
use vesta_supervisor::SupervisorConfig;

const USAGE: &str = "Usage: vesta-supervisor --host <host.toml> --base <directory>";

fn arguments() -> Option<(PathBuf, PathBuf)> {
    let mut args = std::env::args_os().skip(1);
    let (mut host, mut base) = (None, None);
    while let Some(flag) = args.next() {
        let value = PathBuf::from(args.next()?);
        match flag.to_str()? {
            "--host" if host.is_none() => host = Some(value),
            "--base" if base.is_none() => base = Some(value),
            _ => return None,
        }
    }
    Some((host?, base?))
}

fn main() -> ExitCode {
    if std::env::args_os().skip(1).eq([
        std::ffi::OsString::from("update"),
        std::ffi::OsString::from("capabilities"),
    ]) {
        println!(
            "{}",
            vesta_state::update_capabilities(vesta_supervisor::wire::VERSION)
        );
        return ExitCode::SUCCESS;
    }
    let Some((host, base)) = arguments() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let launch = match HostConfiguration::load_file(&host, &base)
        .map_err(|e| e.code())
        .and_then(|config| supervised_launch(&config).map_err(|e| e.code()))
    {
        Ok(launch) => launch,
        Err(code) => {
            eprintln!("supervisor_configuration_failed: {code}");
            return ExitCode::FAILURE;
        }
    };
    if launch.state.prepare().is_err() {
        eprintln!("supervisor_state_unavailable");
        return ExitCode::FAILURE;
    }
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        eprintln!("supervisor_runtime_unavailable");
        return ExitCode::FAILURE;
    };
    let config = SupervisorConfig {
        state: launch.state,
        launch: launch.spec,
        launch_identity: launch.identity,
        restart: launch.restart,
    };
    // Ctrl+C only ends an idle supervisor; a running game keeps it alive.
    // A detached supervisor has no console; if Ctrl+C cannot be registered, never
    // treat that as a stop request.
    let stop = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    match runtime.block_on(vesta_supervisor::run(config, stop)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}
