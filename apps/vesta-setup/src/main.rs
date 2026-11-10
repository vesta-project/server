use std::{path::PathBuf, process::ExitCode};
use vesta_setup::{engine, host, release::Source, ui, visual_states, wizard};

const HELP: &str = "Vesta host setup\n\n  vesta-setup --bundle-url <https-directory> --manifest-sha256 <hex> --release-key <hex>\n  vesta-setup --bundle-dir <directory> --manifest-sha256 <hex> --release-key <hex>\n              [--asset-prefix <target->] [--answers <json-file> --yes] [--no-color] [--ascii]\n  vesta-setup preview [--state <name> | --export <new-directory>] [--no-color] [--ascii]\n  vesta-setup <host|status|stop|activate|setup-code> --root <installation-directory>\n  vesta-setup configure-server --root <installation> --java <absolute-java-path>\n              [--adoption-jar <relative-jar> OR --adoption-argfile <relative-args>] --copied-export\n  vesta-setup update <init|status|check|stage|verify|configure> --root <installation-directory>\n  vesta-setup uninstall --root <installation-directory> [--servers-root <registry>] [--plan]\n  vesta-setup uninstall --server <name|id> --servers-root <registry> [--plan]\n              [--bundle-url <https-directory> --manifest-sha256 <hex> --release-key <hex>\n               --asset-prefix <target->]\n\nInteractive Vesta host setup. Default HTTPS port: 8443.\nUninstall requires interactive confirmation; --plan only previews selected paths.\n";

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "update") {
        return vesta_setup::updates::run(&args[1..]);
    }
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        println!("{HELP}");
        return Ok(());
    }
    let mut values = std::collections::BTreeMap::new();
    let mut flags = std::collections::BTreeSet::new();
    let command = args
        .first()
        .filter(|s| !s.starts_with('-'))
        .map(String::as_str);
    let mut i = usize::from(command.is_some());
    while i < args.len() {
        let key = &args[i];
        if [
            "--yes",
            "--no-color",
            "--ascii",
            "--plan",
            "--copied-export",
        ]
        .contains(&key.as_str())
        {
            if !flags.insert(key.clone()) {
                return Err("setup_duplicate_option".into());
            }
        } else if [
            "--state",
            "--export",
            "--root",
            "--bundle-dir",
            "--bundle-url",
            "--manifest-sha256",
            "--release-key",
            "--answers",
            "--asset-prefix",
            "--server",
            "--servers-root",
            "--java",
            "--adoption-jar",
            "--adoption-argfile",
        ]
        .contains(&key.as_str())
        {
            i += 1;
            let value = args
                .get(i)
                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                .ok_or("setup_missing_option_value")?;
            if values.insert(key.clone(), value.clone()).is_some() {
                return Err("setup_duplicate_option".into());
            }
        } else {
            return Err("setup_unknown_option".into());
        }
        i += 1;
    }
    if command == Some("configure-server") {
        if flags.iter().any(|k| k != "--copied-export")
            || values.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "--root" | "--java" | "--adoption-jar" | "--adoption-argfile"
                )
            })
        {
            return Err("setup_invalid_host_options".into());
        }
        let root = PathBuf::from(values.get("--root").ok_or("setup_missing_option_value")?);
        let java = PathBuf::from(values.get("--java").ok_or("setup_missing_option_value")?);
        host::configure_server(
            &root,
            java,
            values.get("--adoption-jar").cloned(),
            values.get("--adoption-argfile").cloned(),
            flags.contains("--copied-export"),
        )?;
        println!("Local server configuration saved. Restart the pending host to use it.");
        return Ok(());
    }
    if command == Some("uninstall") {
        if values.contains_key("--server") && values.contains_key("--root") {
            return Err("setup_server_uninstall_uses_servers_root_only".into());
        }
        if flags
            .iter()
            .any(|f| f != "--plan" && f != "--no-color" && f != "--ascii")
            || values.contains_key("--answers")
            || values.contains_key("--state")
            || values.contains_key("--export")
        {
            return Err("setup_uninstall_options_invalid_no_confirmation_bypass".into());
        }
        return uninstall(&values, flags.contains("--plan"));
    }
    if matches!(
        command,
        Some("host" | "stop" | "status" | "activate" | "setup-code")
    ) {
        if values.len() != 1 || !values.contains_key("--root") || !flags.is_empty() {
            return Err("setup_invalid_host_options".into());
        }
        let root = PathBuf::from(&values["--root"]);
        return match command {
            Some("setup-code") => {
                let code = host::setup_code(&root)?;
                // Explicit local credential output, never a public descriptor or log.
                println!(
                    "{}",
                    serde_json::json!({"code":code.secret.expose(),"expires_at_ms":code.expires_at_ms,"scope":"first_server"})
                );
                Ok(())
            }
            Some("stop") => host::stop(&root),
            Some("activate") => host::activate(&root),
            Some("status") => {
                let receipt = engine::read_receipt(&root)?;
                println!(
                    "{}",
                    serde_json::json!({"phase":receipt.phase,"installation_id":receipt.installation_id,"pending_host_ready":host::ready(&receipt),"external_access":receipt.external_status})
                );
                Ok(())
            }
            _ => host::serve(&root),
        };
    }
    let mut terminal =
        wizard::Terminal::new(!flags.contains("--no-color"), !flags.contains("--ascii"));
    if command == Some("preview") {
        if values.keys().any(|k| k != "--state" && k != "--export")
            || flags.contains("--yes")
            || values.len() > 1
        {
            return Err("setup_invalid_preview_options".into());
        }
        if let Some(path) = values.get("--export") {
            engine::export_visuals(&PathBuf::from(path))?;
        } else {
            let selected = values.get("--state").map_or("welcome", String::as_str);
            let view = visual_states::all()
                .into_iter()
                .find(|(id, _)| *id == selected)
                .ok_or("setup_unknown_preview_state")?
                .1;
            terminal.show(&view)?;
            println!("  Visual preview · no installation changes");
        }
        return Ok(());
    }
    if command.is_some()
        || values.contains_key("--java")
        || values.contains_key("--adoption-jar")
        || values.contains_key("--adoption-argfile")
        || flags.contains("--copied-export")
        || values
            .keys()
            .any(|k| ["--root", "--state", "--export"].contains(&k.as_str()))
    {
        return Err("setup_invalid_options".into());
    }
    if flags.contains("--plan")
        || values.contains_key("--server")
        || values.contains_key("--servers-root")
    {
        return Err("setup_invalid_options".into());
    }
    let source = Source {
        asset_prefix: values.get("--asset-prefix").cloned(),
        local: values.get("--bundle-dir").map(PathBuf::from),
        base_url: values.get("--bundle-url").cloned(),
        manifest_sha256: values
            .get("--manifest-sha256")
            .cloned()
            .ok_or("setup_manifest_pin_required")?,
        release_key: values
            .get("--release-key")
            .cloned()
            .ok_or("setup_release_key_required")?,
    };
    let answers = match values.get("--answers") {
        Some(path) if flags.contains("--yes") => engine::read_answers(&PathBuf::from(path))?,
        Some(_) => return Err("setup_headless_requires_yes".into()),
        None if flags.contains("--yes") => return Err("setup_yes_requires_answers".into()),
        None => wizard::ask(&mut terminal)?,
    };
    let release = match vesta_setup::release::load(source) {
        Ok(release) => release,
        Err(code) => {
            terminal.show(&engine::failure_view(&code))?;
            return Err(code);
        }
    };
    let result = engine::prepare(&answers, &release, &mut |phase, file, current, total| {
        let _ = terminal.show(&engine::progress_view(phase, file, current, total));
    });
    match result {
        Ok(receipt) => terminal.show(&engine::complete_view(&receipt)),
        Err(code) => {
            terminal.show(&engine::failure_view(&code))?;
            Err(code)
        }
    }
}

fn uninstall(
    values: &std::collections::BTreeMap<String, String>,
    plan: bool,
) -> Result<(), String> {
    use std::process::Command;
    let original_base = std::env::current_dir().map_err(|_| "setup_directory_unavailable")?;
    let absolute = |value: &str| {
        let p = PathBuf::from(value);
        if p.is_absolute() {
            p
        } else {
            original_base.join(p)
        }
    };
    let mut args = vec![std::ffi::OsString::from("uninstall")];
    if let Some(server) = values.get("--server") {
        args.push(server.into());
    } else if !values.contains_key("--root") {
        return Err("setup_uninstall_root_required".into());
    }
    for key in ["--root", "--servers-root"] {
        if let Some(value) = values.get(key) {
            let path = absolute(value);
            engine::guard(&path)?;
            args.extend([std::ffi::OsString::from(key), path.into_os_string()]);
        }
    }
    if plan {
        args.push("--plan".into());
    }
    let executable = if values.contains_key("--bundle-url") || values.contains_key("--bundle-dir") {
        let release = vesta_setup::release::load(Source {
            local: values.get("--bundle-dir").map(PathBuf::from),
            base_url: values.get("--bundle-url").cloned(),
            manifest_sha256: values
                .get("--manifest-sha256")
                .cloned()
                .ok_or("setup_manifest_pin_required")?,
            release_key: values
                .get("--release-key")
                .cloned()
                .ok_or("setup_release_key_required")?,
            asset_prefix: values.get("--asset-prefix").cloned(),
        })?;
        let temporary = std::env::temp_dir().join(format!(
            "vesta-uninstall-tools-{}",
            uuid::Uuid::new_v4().simple()
        ));
        vesta_state::StateDirectory::new(&temporary, None)
            .and_then(|d| d.prepare())
            .map_err(|_| "setup_uninstall_staging_failed")?;
        release.install(&temporary, &mut |_, _, _| {})?;
        temporary
            .join("bin")
            .join(format!("vesta{}", std::env::consts::EXE_SUFFIX))
    } else {
        if values.contains_key("--manifest-sha256")
            || values.contains_key("--release-key")
            || values.contains_key("--asset-prefix")
        {
            return Err("setup_uninstall_bundle_required_with_verification_options".into());
        }
        let root = values.get("--root").map(|v| absolute(v));
        if let Some(root) = root {
            root.join("bin")
                .join(format!("vesta{}", std::env::consts::EXE_SUFFIX))
        } else {
            std::env::current_exe()
                .map_err(|_| "setup_executable_unavailable")?
                .with_file_name(format!("vesta{}", std::env::consts::EXE_SUFFIX))
        }
    };
    engine::guard(&executable)?;
    let removal_roots: Vec<_> = ["--root", "--servers-root"]
        .iter()
        .filter_map(|key| values.get(*key).map(|v| absolute(v)))
        .collect();
    let mut destination = original_base.as_path();
    while removal_roots.iter().any(|root| {
        let current = vesta_setup::paths::comparable(destination);
        let root_key = vesta_setup::paths::comparable(root);
        current == root_key || current.starts_with(&(root_key + "/"))
    }) {
        destination = destination.parent().ok_or("setup_uninstall_root_invalid")?;
    }
    if destination != original_base {
        std::env::set_current_dir(destination).map_err(|_| "setup_directory_unavailable")?;
    }
    let mut command = Command::new(&executable);
    command.args(args);
    #[cfg(windows)]
    if let Some(root) = values.get("--root").map(|v| absolute(v)) {
        let own_executable = std::env::current_exe().map_err(|_| "setup_executable_unavailable")?;
        let own_key = vesta_setup::paths::comparable(&own_executable);
        let root_key = vesta_setup::paths::comparable(&root);
        if own_key == root_key || own_key.starts_with(&(root_key + "/")) {
            command.env("VESTA_UNINSTALL_SETUP_PID", std::process::id().to_string());
        }
    }
    let status = command
        .status()
        .map_err(|_| "setup_uninstall_cli_unavailable")?;
    if status.code() == Some(75) {
        return Err("setup_uninstall_handed_off".into());
    }
    if status.success() {
        Ok(())
    } else {
        Err("setup_uninstall_incomplete_or_cancelled".into())
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error == "setup_uninstall_handed_off" => ExitCode::from(75),
        Err(error) if error == "setup_cancelled" => {
            println!(
                "{}",
                ui::render(
                    &engine::cancelled_view(),
                    ui::Theme {
                        color: false,
                        unicode: true,
                        width: 80
                    }
                )
            );
            ExitCode::from(130)
        }
        Err(error) => {
            eprintln!("Vesta setup: {error}");
            ExitCode::FAILURE
        }
    }
}
