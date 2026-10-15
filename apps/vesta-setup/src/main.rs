use std::{path::PathBuf, process::ExitCode};
use vesta_setup::{engine, host, release::Source, ui, visual_states, wizard};

const HELP: &str = "Vesta host setup\n\n  vesta-setup --bundle-url <https-directory> --manifest-sha256 <hex> --release-key <hex>\n  vesta-setup --bundle-dir <directory> --manifest-sha256 <hex> --release-key <hex>\n              [--answers <json-file> --yes] [--no-color] [--ascii]\n  vesta-setup preview [--state <name> | --export <new-directory>] [--no-color] [--ascii]\n  vesta-setup <host|status|stop|activate> --root <installation-directory>\n\nInteractive setup prepares a host for app onboarding. Minecraft and Java selection remain in the app.\n";

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        if ["--yes", "--no-color", "--ascii"].contains(&key.as_str()) {
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
    if matches!(command, Some("host" | "stop" | "status" | "activate")) {
        if values.len() != 1 || !values.contains_key("--root") || !flags.is_empty() {
            return Err("setup_invalid_host_options".into());
        }
        let root = PathBuf::from(&values["--root"]);
        return match command {
            Some("stop") => host::stop(&root),
            Some("activate") => host::activate(&root),
            Some("status") => {
                let receipt = engine::read_receipt(&root)?;
                println!(
                    "{}",
                    serde_json::json!({"phase":receipt.phase,"installation_id":receipt.installation_id,"pending_host_ready":host::ready(&receipt)})
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
        || values
            .keys()
            .any(|k| ["--root", "--state", "--export"].contains(&k.as_str()))
    {
        return Err("setup_invalid_options".into());
    }
    let source = Source {
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
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
