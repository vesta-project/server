//! Thin trusted-local configuration inspection entrypoint, not a management host.
//!
//! No listener, database preparation, owner bootstrap or game start is performed.
use std::{path::PathBuf, process::ExitCode};
use vesta_control::config::HostConfiguration;

const USAGE: &str = "Usage: vesta-agent check-config <path>";

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
    if command != "check-config" {
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
    match HostConfiguration::load_file(&source, &base)
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
