//! Command dispatch with fixed redacted failures. Never prints config contents.
use crate::{
    cli::{Command, NamedOp},
    control, editor, local_files, serve, servers, status,
};
use std::{path::Path, process::ExitCode};
use vesta_control::config::HostConfiguration;
pub async fn run(command: Command, base: &Path) -> ExitCode {
    let result = match command {
        Command::Help => {
            println!("{}", crate::cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!(
                "vesta-agent {}; updates_are_not_signed_yet",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Command::Update => {
            println!(
                "current_version={}; available_version=unknown; signed_update_catalogue_not_implemented",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("update_refused: signed_updates_not_implemented_F41; no_binary_replacement");
            return ExitCode::FAILURE;
        }
        Command::Check(source) => {
            return match HostConfiguration::load_file(&source, base)
                .and_then(|c| c.validate_host_paths())
            {
                Ok(_) => {
                    println!("configuration_valid; runtime_and_transport_not_started");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("configuration_failed: {}", e.code());
                    ExitCode::FAILURE
                }
            };
        }
        Command::Status(source) => return status::run(&source, base),
        Command::Serve { source, local_only } => {
            return serve::run(&source, base, local_only).await;
        }
        Command::Create(options) => servers::create(options, base).await,
        Command::Delete {
            name,
            confirm,
            root,
        } => servers::root(root.as_deref(), base)
            .and_then(|root| servers::delete(&root, &name, &confirm))
            .map(|recovery| {
                println!(
                    "deleted_by_recoverable_move; no_files_erased; recovery={}",
                    recovery.display()
                );
            }),
        Command::ResolveLaunch {
            name,
            confirm,
            root,
        } => servers::root(root.as_deref(), base)
            .and_then(|root| crate::supervision::resolve_launch(&root, &name, &confirm))
            .map(|()| println!("launch_intent_resolved; no_process_touched")),
        Command::BackupInit { name, root } => servers::root(root.as_deref(), base)
            .and_then(|root| crate::supervision::backup_init(&root, &name))
            .map(|()| println!("backup_repository_initialized")),
        Command::ResetRestarts { name, root } => servers::root(root.as_deref(), base)
            .and_then(|root| crate::supervision::reset_restarts(&root, &name))
            .map(|()| println!("crash_restarts_reset; no_process_touched")),
        Command::Management { name, root, op } => management(name, root.as_deref(), op, base).await,
        Command::Named { op, name, root } => named(op, name, root.as_deref(), base).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("local_command_failed: {code}");
            ExitCode::FAILURE
        }
    }
}
async fn named(
    op: NamedOp,
    name: String,
    selected_root: Option<&Path>,
    base: &Path,
) -> local_files::Result<()> {
    let root = servers::root(selected_root, base)?;
    let server = servers::resolve(&root, &name)?;
    if op == NamedOp::Config {
        return editor::open(&server.config);
    }
    let configuration = HostConfiguration::load_file(&server.config, base)
        .map_err(|_| "local_config_unavailable")?;
    // Request contains only a closed enum and name; never executable, args or cwd.
    let op = match op {
        NamedOp::Start => control::Op::Start,
        NamedOp::Stop => control::Op::Stop,
        NamedOp::Restart => control::Op::Restart,
        NamedOp::Config => unreachable!(),
    };
    control::request(configuration.state(), name, op).await?;
    println!("local_game_operation_complete; readiness_not_observed");
    Ok(())
}

async fn management(
    name: String,
    selected_root: Option<&Path>,
    op: control::Op,
    base: &Path,
) -> local_files::Result<()> {
    let root = servers::root(selected_root, base)?;
    let server = servers::resolve(&root, &name)?;
    let configuration = HostConfiguration::load_file(&server.config, base)
        .map_err(|_| "local_config_unavailable")?;
    let data = control::request_data(configuration.state(), name, op)
        .await?
        .ok_or("local_control_invalid_response")?;
    // The one explicit secret presentation. No request/response Debug logging.
    println!(
        "{}",
        serde_json::to_string(&data).map_err(|_| "local_control_invalid_response")?
    );
    Ok(())
}
