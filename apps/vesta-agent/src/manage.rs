//! A scoped local client, not a second process owner or an operating-system shell.
use crate::{
    cli::{self, Command},
    local_files,
    output::Format,
    servers,
};
use std::{
    ffi::OsString,
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

pub const USAGE: &str = "Server management\n\n  vesta manage <server_id|name> [command ...] [--install-root <root>]\n\n  start | stop | restart       Minecraft lifecycle\n  status                      Live agent and Minecraft status\n  player_list                 Fresh bounded player query; unavailable stays unknown\n  config                      Open this server's configuration\n  pair [--role role] [--expires minutes]\n  devices list | revoke <device-id>\n  owner list | link --minecraft-uuid <uuid> [--role role]\n  owner unlink --minecraft-uuid <uuid> [--revoke-devices]\n  access show | set [access flags]\n  superadmin grant|revoke <user-id>\n  backup init | resolve [<job-id> --confirm <job-id>]\n  recovery export --output <file> [--passphrase-file <file>]\n  recovery import --input <file> --confirm <identity-id> [--passphrase-file <file>]\n  resolve-launch --confirm <server_id>\n  reset-restarts\n  delete --confirm <server_id>\n\n  help                        Show these commands\n  exit | quit | back           Return to your terminal\n\nThe prompt stays bound to one server. Exit/EOF does not stop it.\nCtrl+C and Ctrl+D leave a supported line-editor terminal.\nQuote paths containing spaces; backslashes remain literal. No OS shell expansion.\nCommands are not saved to history. Direct commands return their normal exit code.";

pub fn parse_list(args: &[OsString]) -> Result<Command, ()> {
    let mut root = None;
    let mut json = false;
    let mut at = if args[0] == "servers" && args.get(1).is_some_and(|v| v == "list") {
        2
    } else {
        1
    };
    while at < args.len() {
        match args[at].to_str() {
            Some("--json") if !json => json = true,
            Some("--install-root") if root.is_none() => {
                at += 1;
                root = Some(root_value(args.get(at))?);
            }
            _ => return Err(()),
        }
        at += 1;
    }
    Ok(Command::List { root, json })
}
fn root_value(value: Option<&OsString>) -> Result<PathBuf, ()> {
    value
        .filter(|v| !v.is_empty() && !v.to_string_lossy().starts_with('-'))
        .map(PathBuf::from)
        .ok_or(())
}
pub fn parse(args: &[OsString]) -> Result<Command, ()> {
    if args.len() == 2 && matches!(args[1].to_str(), Some("help" | "--help" | "-h")) {
        return Ok(Command::ManageHelp);
    }
    let mut body = Vec::new();
    let mut root = None;
    let mut at = 1;
    while at < args.len() {
        if args[at] == "--install-root" {
            if root.is_some() {
                return Err(());
            }
            at += 1;
            root = Some(root_value(args.get(at))?);
        } else {
            body.push(args[at].clone());
        }
        at += 1;
    }
    let target = body
        .first()
        .and_then(|v| v.to_str())
        .filter(|v| cli::valid_name(v) || servers::valid_instance_id(v))
        .ok_or(())?
        .to_owned();
    Ok(Command::Manage {
        target,
        root,
        args: body.into_iter().skip(1).collect(),
    })
}

pub fn list(root: Option<&Path>, base: &Path, json: bool) -> local_files::Result<()> {
    let listed = servers::list(&servers::root(root, base)?)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&listed).map_err(|_| "local_registry_unavailable")?
        );
    } else if listed.is_empty() {
        println!("No registered servers.");
    } else {
        println!("{:<36}  NAME", "SERVER ID");
        for entry in listed {
            println!("{}  {}", entry.id, entry.name);
        }
    }
    Ok(())
}

struct Scope {
    root: PathBuf,
    name: String,
    id: String,
}
impl Scope {
    fn verify(&self) -> local_files::Result<()> {
        let (_, current, _) = servers::select(&self.root, &self.name)?;
        if current != self.id {
            return Err("local_server_identity_changed");
        }
        Ok(())
    }
    fn command(&self, args: &[OsString]) -> Result<Command, ()> {
        if args.iter().any(|v| v == "--install-root") {
            return Err(());
        }
        let mut args = args.to_vec();
        let first = args
            .first()
            .and_then(|s| s.to_str())
            .ok_or(())?
            .replace('_', "-");
        args[0] = first.clone().into();
        if matches!(first.as_str(), "backup" | "recovery") {
            let second = args.get(1).and_then(|s| s.to_str()).ok_or(())?;
            args[0] = format!("{first}-{second}").into();
            args.remove(1);
        }
        let op = args[0].to_str().ok_or(())?;
        if matches!(op, "status" | "player-list" | "players") {
            if args.len() != 1 {
                return Err(());
            }
            return Ok(Command::Management {
                name: self.name.clone(),
                root: Some(self.root.clone()),
                op: if op == "status" {
                    crate::control::Op::Status
                } else {
                    crate::control::Op::PlayersList
                },
            });
        }
        let position = match op {
            "uninstall" => 1,
            "config" | "start" | "stop" | "restart" | "pair" | "backup-init" | "backup-resolve"
            | "recovery-export" | "recovery-import" | "delete" | "resolve-launch"
            | "reset-restarts" => 1,
            "access" | "owner" => 2,
            "devices" => {
                if args.get(1).is_some_and(|s| s == "revoke") {
                    3
                } else {
                    2
                }
            }
            "superadmin" => 3,
            _ => return Err(()),
        };
        if position > args.len() {
            return Err(());
        }
        if matches!(op, "delete" | "resolve-launch") {
            let at = args.iter().position(|s| s == "--confirm").ok_or(())? + 1;
            if args.get(at).and_then(|s| s.to_str()) != Some(&self.id) {
                return Err(());
            }
            args[at] = self.name.clone().into();
        }
        args.insert(position, self.name.clone().into());
        args.extend([
            OsString::from("--install-root"),
            self.root.as_os_str().to_owned(),
        ]);
        cli::parse(args)
    }
    async fn execute(&self, args: &[OsString], base: &Path, default_format: Format) -> ExitCode {
        let (args, format) = match presentation(args, default_format) {
            Ok(value) => value,
            Err(()) => {
                eprintln!("Use --json at most once per command.");
                return ExitCode::from(2);
            }
        };
        if args.len() == 1 && matches!(args[0].to_str(), Some("exit" | "quit" | "back")) {
            return ExitCode::SUCCESS;
        }
        if args.len() == 1 && matches!(args[0].to_str(), Some("help" | "--help" | "-h")) {
            println!(
                "{USAGE}\n\nTerminal replies are formatted. Append --json for structured output."
            );
            return ExitCode::SUCCESS;
        }
        if let Err(code) = self.verify() {
            eprintln!("local_command_failed: {code}");
            return ExitCode::FAILURE;
        }
        match self.command(&args) {
            Ok(command) => Box::pin(crate::commands::run_with_format(command, base, format)).await,
            Err(()) => {
                eprintln!("Invalid server command. Use help for this server's commands.");
                ExitCode::from(2)
            }
        }
    }
}

fn presentation(args: &[OsString], default_format: Format) -> Result<(Vec<OsString>, Format), ()> {
    let count = args.iter().filter(|arg| *arg == "--json").count();
    if count > 1 {
        return Err(());
    }
    Ok((
        args.iter()
            .filter(|arg| *arg != "--json")
            .cloned()
            .collect(),
        if count == 1 {
            Format::Json
        } else {
            default_format
        },
    ))
}

pub async fn run(target: &str, root: Option<&Path>, args: &[OsString], base: &Path) -> ExitCode {
    let selected = servers::root(root, base).and_then(|root| {
        let (name, id, _) = servers::select(&root, target)?;
        Ok(Scope { root, name, id })
    });
    let scope = match selected {
        Ok(scope) => scope,
        Err(code) => {
            eprintln!("local_command_failed: {code}");
            return ExitCode::FAILURE;
        }
    };
    let format = if io::stdout().is_terminal() {
        Format::Human
    } else {
        Format::Json
    };
    if !args.is_empty() {
        return scope.execute(args, base, format).await;
    }
    let terminal = io::stdin().is_terminal() && io::stdout().is_terminal();
    if terminal {
        let brand = if std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").as_deref() != Ok("dumb")
        {
            "\x1b[1;36mVESTA\x1b[0m"
        } else {
            "VESTA"
        };
        println!(
            "\n{brand}  /  {}\n{}\n\nType help for commands; exit to leave management.\n",
            scope.name, scope.id
        );
    }
    let config = rustyline::Config::builder().auto_add_history(false).build();
    let mut editor = if terminal {
        match rustyline::DefaultEditor::with_config(config) {
            Ok(editor) => Some(editor),
            Err(_) => {
                eprintln!("local_terminal_unavailable");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let prompt = format!("{} [{}]> ", scope.name, &scope.id[scope.id.len() - 8..]);
    let mut result = ExitCode::SUCCESS;
    loop {
        let line = if let Some(editor) = &mut editor {
            match editor.readline(&prompt) {
                Ok(line) => line,
                Err(
                    rustyline::error::ReadlineError::Eof
                    | rustyline::error::ReadlineError::Interrupted,
                ) => break,
                Err(_) => {
                    eprintln!("local_terminal_read_failed");
                    result = ExitCode::FAILURE;
                    break;
                }
            }
        } else {
            let mut line = String::new();
            // Do not hold StdinLock while the terminal editor owns console IO.
            match io::stdin().lock().take(8193).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.len() <= 8192 => line,
                _ => {
                    eprintln!("local_terminal_input_invalid_or_too_long");
                    result = ExitCode::FAILURE;
                    break;
                }
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let args = match words(&line) {
            Ok(args) => args,
            Err(()) => {
                eprintln!("Invalid input. Close quotes and keep commands under 8192 bytes.");
                result = ExitCode::from(2);
                continue;
            }
        };
        if args.len() == 1 && matches!(args[0].to_str(), Some("exit" | "quit" | "back")) {
            break;
        }
        let code = scope.execute(&args, base, format).await;
        if code != ExitCode::SUCCESS {
            result = code;
        }
        let _ = io::stdout().flush();
    }
    if terminal {
        println!("Left {}. Management session closed.", scope.name);
    }
    result
}

/// Quotes group arguments on both platforms; a Windows backslash is never eaten.
fn words(line: &str) -> Result<Vec<OsString>, ()> {
    if line.len() > 8192
        || line
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    {
        return Err(());
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for c in line.trim_end_matches(['\r', '\n']).chars() {
        if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if quote.is_none() && c.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word).into());
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if quote.is_some() {
        return Err(());
    }
    if started {
        words.push(word.into());
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_override_is_per_command_and_does_not_change_scope_or_arguments() {
        let (command, format) = presentation(&args(&["status", "--json"]), Format::Human).unwrap();
        assert_eq!(command, args(&["status"]));
        assert!(format == Format::Json);
        assert!(presentation(&args(&["status"]), Format::Human).unwrap().1 == Format::Human);
        assert!(presentation(&args(&["status"]), Format::Json).unwrap().1 == Format::Json);
        assert!(presentation(&args(&["start", "--json", "--json"]), Format::Human).is_err());
        assert!(
            scope()
                .command(
                    &presentation(
                        &args(&["start", "--install-root", "other", "--json"]),
                        Format::Human
                    )
                    .unwrap()
                    .0
                )
                .is_err()
        );
    }
    fn args(text: &[&str]) -> Vec<OsString> {
        text.iter().map(OsString::from).collect()
    }
    fn scope() -> Scope {
        Scope {
            root: std::env::temp_dir().join("vesta-scoped-command-fixture"),
            name: "owned".into(),
            id: format!("srv_{}", "a".repeat(32)),
        }
    }
    #[test]
    fn scoped_grammar_injects_only_the_selected_server() {
        let scope = scope();
        for text in [
            vec!["start"],
            vec!["stop"],
            vec!["restart"],
            vec!["config"],
            vec!["status"],
            vec!["player_list"],
            vec!["pair", "--role", "viewer"],
            vec!["devices", "list"],
            vec!["devices", "revoke", "device"],
            vec!["owner", "list"],
            vec!["access", "show"],
            vec!["backup", "init"],
            vec!["recovery", "export", "--output", "file with spaces.json"],
        ] {
            let parsed = scope.command(&args(&text)).unwrap();
            match parsed {
                Command::Named { name, root, .. }
                | Command::Management { name, root, .. }
                | Command::BackupInit { name, root }
                | Command::RecoveryExport { name, root, .. } => {
                    assert_eq!(name, scope.name);
                    assert_eq!(root, Some(scope.root.clone()));
                }
                _ => panic!("unexpected scoped dispatch"),
            }
        }
        for text in [
            vec!["start", "other"],
            vec!["manage", "other"],
            vec!["serve", "x"],
            vec!["create", "other"],
            vec!["start", "--install-root", "other"],
            vec!["status", "x"],
            vec!["delete", "--confirm", "owned"],
        ] {
            assert!(scope.command(&args(&text)).is_err());
        }
        assert!(
            matches!(scope.command(&args(&["delete", "--confirm", &scope.id])).unwrap(), Command::Delete { name, confirm, .. } if name == "owned" && confirm == "owned")
        );
    }
    #[test]
    fn terminal_quoting_preserves_windows_and_unix_paths_without_shell_expansion() {
        assert_eq!(
            words(r#"recovery export --output "C:\Server Data\state.json""#).unwrap(),
            args(&[
                "recovery",
                "export",
                "--output",
                r"C:\Server Data\state.json"
            ])
        );
        assert_eq!(
            words("recovery export --output '/srv/server data/state.json'").unwrap(),
            args(&[
                "recovery",
                "export",
                "--output",
                "/srv/server data/state.json"
            ])
        );
        assert_eq!(words("config; echo rejected").unwrap()[0], "config;");
        assert!(words("'unclosed").is_err());
        assert!(words(&"a".repeat(8193)).is_err());
        assert!(words("help\u{1b}[0m").is_err());
    }
    #[test]
    fn management_entry_and_registry_flags_are_closed_and_bounded() {
        assert!(
            matches!(cli::parse(args(&["manage", "owned"])).unwrap(), Command::Manage { args, .. } if args.is_empty())
        );
        assert!(matches!(
            cli::parse(args(&["servers", "list", "--json"])).unwrap(),
            Command::List { json: true, .. }
        ));
        for text in [
            vec!["manage"],
            vec!["manage", "../other"],
            vec!["list", "--json", "--json"],
            vec!["list", "--install-root"],
            vec![
                "manage",
                "owned",
                "--install-root",
                "a",
                "--install-root",
                "b",
            ],
        ] {
            assert!(cli::parse(args(&text)).is_err());
        }
    }
}
