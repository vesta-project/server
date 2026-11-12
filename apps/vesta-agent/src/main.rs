// Trusted OS-local CLI entrypoint. Shared by the vesta and vesta-agent binaries.
use std::process::ExitCode;
mod agent_startup;
mod cli;
mod commands;
mod control;
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
