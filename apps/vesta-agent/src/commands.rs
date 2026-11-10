//! Command dispatch with fixed redacted failures. Never prints config contents.
use crate::{
    cli::{Command, NamedOp},
    control, editor, local_files,
    output::Format,
    serve, servers, status,
};
use std::{path::Path, process::ExitCode};
use vesta_control::config::HostConfiguration;
pub async fn run(command: Command, base: &Path) -> ExitCode {
    run_with_format(command, base, Format::Json).await
}
pub async fn run_with_format(command: Command, base: &Path, format: Format) -> ExitCode {
    let result = match command {
        Command::UninstallHelp => {
            println!("{}", crate::uninstall::USAGE);
            return ExitCode::SUCCESS;
        }
        Command::Uninstall(request) => return crate::uninstall::run(request, base).await,
        Command::UninstallExecute { job, token } => {
            return crate::uninstall::run_job(job, token).await;
        }
        Command::Shutdown(source) => {
            let source = match local_files::absolute(&source, base) {
                Ok(p) => p,
                Err(code) => {
                    eprintln!("{code}");
                    return ExitCode::FAILURE;
                }
            };
            match HostConfiguration::load_file(&source, base) {
                Ok(c) => match servers::control_name(&source) {
                    Ok(name) => control::request(c.state(), name, control::Op::Shutdown).await,
                    Err(code) => Err(code),
                },
                Err(_) => Err("local_config_unreadable"),
            }
        }
        Command::Help => {
            println!("{}\n\n{}", crate::cli::USAGE, crate::uninstall::USAGE);
            return ExitCode::SUCCESS;
        }
        Command::ManageHelp => {
            println!(
                "{}\n\n  uninstall  Remove this server with name and final confirmation\n\nTerminal replies are formatted. Append --json for structured output.",
                crate::manage::USAGE
            );
            return ExitCode::SUCCESS;
        }
        Command::CreateHelp => {
            println!(
                "vesta create <name> --install-root <root> --minecraft-version <version> --java <path>\n  [--loader vanilla|paper|folia|purpur|fabric|quilt] [--loader-version <version>]\n  [--bind <loopback-ip>] [--port <port>] [--approve-install]\n  [--ack-warnings] [--ack-legacy-checksum]\n  [--agent-startup manual|boot] (prompted in an interactive terminal; required headless)"
            );
            return ExitCode::SUCCESS;
        }
        Command::List { root, json } => crate::manage::list(root.as_deref(), base, json),
        Command::Manage { target, root, args } => {
            return crate::manage::run(&target, root.as_deref(), &args, base).await;
        }
        Command::Version => {
            println!(
                "vesta-agent {}; signed_release_packages_supported; self_update_os_local_fenced",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Command::Update(args) => {
            if !args.is_empty() {
                return match vesta_setup::updates::run(&args) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(code) => {
                        eprintln!("{code}");
                        ExitCode::FAILURE
                    }
                };
            }
            println!(
                "current_version={}; available_version=unknown; signed_update_catalogue_not_implemented",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!(
                "update_refused: self_update_activation_not_implemented; no_binary_replacement"
            );
            eprintln!("Signed update check/stage/verify and startup policy: vesta update --help");
            return ExitCode::FAILURE;
        }
        Command::Check(source) => {
            return match HostConfiguration::load_file(&source, base)
                .map_err(vesta_control::host::HostError::Config)
                .and_then(|c| vesta_control::host::validate_launch(&c))
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
        Command::Serve {
            source,
            local_only,
            allow_local,
        } => {
            return serve::run(&source, base, local_only, allow_local).await;
        }
        Command::AgentStartup { name, root, choice } => {
            crate::agent_startup::command(&root, &name, choice)
        }
        Command::AgentStart { name, root } => crate::agent_startup::start(&root, &name),
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
        Command::RecoveryExport {
            name,
            output,
            passphrase_file,
            root,
        } => servers::root(root.as_deref(), base)
            .and_then(|root| {
                crate::recovery::recovery_export(
                    &root,
                    &name,
                    &output,
                    base,
                    passphrase_file.as_deref(),
                )
            })
            .map(|summary| {
                println!(
                    "recovery_export_written tables={} rows={}",
                    summary.tables, summary.rows
                )
            }),
        Command::RecoveryImport {
            name,
            input,
            passphrase_file,
            confirm,
            root,
        } => servers::root(root.as_deref(), base)
            .and_then(|root| {
                crate::recovery::recovery_import(
                    &root,
                    &name,
                    &input,
                    passphrase_file.as_deref(),
                    &confirm,
                    base,
                )
            })
            .map(|summary| {
                println!(
                    "recovery_imported server_id={} tables={} rows={}",
                    summary.server_id, summary.tables, summary.rows
                )
            }),
        Command::BackupResolve {
            name,
            job,
            confirm,
            root,
        } => servers::root(root.as_deref(), base).and_then(|root| {
            crate::recovery::backup_resolve(&root, &name, job.as_deref(), confirm.as_deref())
        }),
        Command::ResetRestarts { name, root } => servers::root(root.as_deref(), base)
            .and_then(|root| crate::supervision::reset_restarts(&root, &name))
            .map(|()| println!("crash_restarts_reset; no_process_touched")),
        Command::Management { name, root, op } => {
            management(name, root.as_deref(), op, base, format).await
        }
        Command::Named { op, name, root } => named(op, name, root.as_deref(), base, format).await,
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
    format: Format,
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
    control::request(configuration.state(), name, op.clone()).await?;
    if format == Format::Human {
        println!(
            "{} completed. Run status to check Minecraft readiness.",
            match op {
                control::Op::Start => "Start",
                control::Op::Stop => "Stop",
                control::Op::Restart => "Restart",
                _ => unreachable!(),
            }
        );
    } else {
        println!("local_game_operation_complete; readiness_not_observed");
    }
    Ok(())
}

async fn management(
    name: String,
    selected_root: Option<&Path>,
    op: control::Op,
    base: &Path,
    format: Format,
) -> local_files::Result<()> {
    let root = servers::root(selected_root, base)?;
    let server = servers::resolve(&root, &name)?;
    let configuration = HostConfiguration::load_file(&server.config, base)
        .map_err(|_| "local_config_unavailable")?;
    let data = control::request_data(configuration.state(), name, op.clone())
        .await?
        .ok_or("local_control_invalid_response")?;
    // The one explicit secret presentation. No request/response Debug logging.
    println!("{}", crate::output::management(format, &op, &data)?);
    Ok(())
}
