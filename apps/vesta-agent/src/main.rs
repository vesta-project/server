// Trusted OS-local CLI entrypoint. Shared by the vesta and vesta-agent binaries.
use std::process::ExitCode;
mod agent_startup;
mod cli;
mod commands;
mod control;
mod host_local;
mod host_mode;
#[cfg(windows)]
mod host_local_windows;
#[cfg(test)]
mod control_tests;
mod editor;
mod local_files;
mod manage;
mod output;
mod recovery;
#[cfg(windows)]
use vesta_supervisor::pipe_security;
mod serve;
mod servers;
mod sessions;
mod status;
mod supervision;
mod uninstall;
mod uninstall_fs;
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
    let dispatch_args: Vec<_> = std::env::args_os().skip(1).collect();
    let name = if std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|n| n == "vesta"))
        .unwrap_or(false)
    {
        "vesta"
    } else {
        "vesta-agent"
    };
    match vesta_setup::updates::dispatch_active(name, &dispatch_args) {
        Ok(Some(status)) => {
            return ExitCode::from(
                status
                    .code()
                    .and_then(|c| u8::try_from(c).ok())
                    .unwrap_or(1),
            );
        }
        Err(code) => {
            eprintln!("{code}");
            return ExitCode::FAILURE;
        }
        Ok(None) => (),
    }
    #[cfg(target_os = "linux")]
    if dispatch_args.first().and_then(|s| s.to_str()).is_some_and(|s| {
        s == "import-root"
            || s == "uninstall"
                && (dispatch_args.iter().any(|a| a == "--keep-data" || a == "--remove-data")
                    || std::path::Path::new(vesta_setup::linux_install::DATA_ROOT_RECORD).exists())
    }) {
        return vesta_setup::linux_install::admin(&dispatch_args);
    }
    if dispatch_args
        .first()
        .and_then(|a| a.to_str())
        .is_some_and(|a| matches!(a, "setup-code" | "link" | "host-status"))
    {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            eprintln!("host_local_runtime_unavailable");
            return ExitCode::FAILURE;
        };
        return runtime.block_on(host_local::run(dispatch_args));
    }
    if dispatch_args.first().and_then(|s|s.to_str())==Some("host") {
        if dispatch_args.len()!=2 {eprintln!("usage: vesta-agent host <host.toml>");return ExitCode::from(2)}
        let runtime=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("host runtime");
        return runtime.block_on(host_mode::run(std::path::Path::new(&dispatch_args[1])));
    }
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(()) => {
            eprintln!("{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let Ok(base) = std::env::current_dir() else {
        eprintln!("configuration_failed: config_environment_unavailable");
        return ExitCode::FAILURE;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        eprintln!("local_runtime_unavailable");
        return ExitCode::FAILURE;
    };
    runtime.block_on(commands::run(command, &base))
}
