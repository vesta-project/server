//! Development builds, one-command agent updates and an optional tools shell.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

const BINARIES: [&str; 4] = ["vesta", "vesta-agent", "vesta-supervisor", "vesta-setup"];
mod bootstrap;
#[cfg(windows)]
mod console;
#[cfg(windows)]
mod detached;
mod restart;
mod session;
const HELP: &str = "Vesta development\n\n  ./dev.ps1                         Build and update dev-server (create if missing), then return\n  ./dev.ps1 restart [server]        Build and update a selected server\n  ./dev.ps1 build                   Build tools without restarting\n  ./dev.ps1 shell                   Open the optional development shell\n  ./dev.ps1 pair                    Print a new owner pairing link\n\nOptions: --release, --server-root <registry>, --local-only, --configured-listener.\nDefault server data: <checkout>/.dev/servers (never Temp).\nFirst-run setup uses Minecraft 1.21.1 and JAVA_HOME or VESTA_DEV_JAVA.\nVESTA_DEV_MINECRAFT_VERSION overrides the first-run version.\nListener overrides apply to updates. The current listener and running/stopped\nMinecraft state are preserved. An offline agent starts with its configured listener.\n\nLinux: cargo dev update [server], cargo dev build, or cargo dev for a shell.\nBuild failures leave the running agent alone. Updates reuse a checkout-local\nstaging session; unrelated servers are not restarted.\n";

#[derive(Default)]
struct Options {
    build_only: bool,
    update: bool,
    release: bool,
    session: Option<PathBuf>,
    server_root: Option<PathBuf>,
    restart: Option<String>,
    local_only: Option<bool>,
    pair: bool,
}

fn options(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut result = Options::default();
    let mut args = args.into_iter().peekable();
    if let Some(command) = args.peek().and_then(|v| v.to_str()) {
        match command {
            "build" => {
                result.build_only = true;
                args.next();
            }
            "update" | "restart" => {
                result.build_only = true;
                result.update = true;
                args.next();
            }
            "shell" => {
                args.next();
            }
            "pair" => {
                result.build_only = true;
                result.pair = true;
                args.next();
            }
            _ => {}
        }
    }
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--release") if !result.release => result.release = true,
            Some("--server-root") if result.server_root.is_none() => {
                result.server_root = Some(args.next().ok_or("missing server root")?.into());
            }
            Some("--session") if result.build_only && result.session.is_none() => {
                result.session = Some(args.next().ok_or("missing build session")?.into());
            }
            Some("--restart") if result.restart.is_none() => {
                result.restart = Some(
                    args.next()
                        .and_then(|s| s.into_string().ok())
                        .filter(|s| !s.is_empty() && !s.starts_with('-'))
                        .ok_or("--restart requires a server ID or name")?,
                );
            }
            Some("--local-only") if result.local_only.is_none() => result.local_only = Some(true),
            Some("--configured-listener") if result.local_only.is_none() => {
                result.local_only = Some(false)
            }
            Some(target)
                if result.update
                    && result.restart.is_none()
                    && !target.starts_with('-')
                    && !target.is_empty() =>
            {
                result.restart = Some(target.to_owned());
            }
            _ => {
                return Err(format!(
                    "invalid development option: {}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    if result.update && result.restart.is_none() {
        result.restart = Some("dev-server".into());
    }
    if result.restart.is_some()
        && !result.update
        && (!result.build_only || result.session.is_none())
    {
        return Err(
            "--restart is only available through vesta-rebuild inside a development session".into(),
        );
    }
    if result.local_only.is_some() && result.restart.is_none() {
        return Err("listener overrides require --restart <server>".into());
    }
    if result.pair && (result.restart.is_some() || result.session.is_some()) {
        return Err("pair does not restart a server or select a build session".into());
    }
    Ok(result)
}

fn artifact(value: &serde_json::Value) -> Option<(&str, PathBuf)> {
    if value["reason"] != "compiler-artifact"
        || !value["target"]["kind"]
            .as_array()?
            .iter()
            .any(|v| v == "bin")
    {
        return None;
    }
    let name = value["target"]["name"].as_str()?;
    BINARIES
        .contains(&name)
        .then_some((name, value["executable"].as_str()?.into()))
}

fn build(repo: &Path, release: bool) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(repo).args([
        "build",
        "--locked",
        "-p",
        "vesta-dev",
        "-p",
        "vesta-agent",
        "-p",
        "vesta-setup",
        "--bins",
        "--message-format=json-render-diagnostics",
    ]);
    if release {
        command.arg("--release");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut artifacts = BTreeMap::new();
    let mut invalid = false;
    for line in BufReader::new(child.stdout.take().expect("piped build output")).lines() {
        match line
            .ok()
            .and_then(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        {
            Some(value) => {
                if let Some((name, executable)) = artifact(&value) {
                    artifacts.insert(name.to_owned(), executable);
                }
                if let Some(rendered) = value["message"]["rendered"].as_str() {
                    eprint!("{rendered}");
                }
            }
            None => invalid = true,
        }
    }
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("build failed; the previous session build remains selected".into());
    }
    if invalid || BINARIES.iter().any(|name| !artifacts.contains_key(*name)) {
        return Err(
            "build did not report all four executables; previous selection retained".into(),
        );
    }
    Ok(artifacts)
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn stage(session: &Path, artifacts: &BTreeMap<String, PathBuf>) -> Result<PathBuf, String> {
    if let Some(bin) = session::unchanged_build(session, artifacts) {
        return Ok(bin);
    }
    // New directories avoid replacing executables held open by a Windows agent/supervisor.
    // Publish only after the entire set has copied successfully.
    let bin = session
        .join("builds")
        .join(uuid::Uuid::new_v4().to_string());
    let publish = || -> std::io::Result<()> {
        fs::create_dir_all(&bin)?;
        for name in BINARIES {
            let source = artifacts
                .get(name)
                .ok_or_else(|| std::io::Error::other("missing executable"))?;
            fs::copy(source, bin.join(executable(name)))?;
        }
        let pending = session.join(format!("current-bin-{}.tmp", uuid::Uuid::new_v4()));
        fs::write(
            &pending,
            bin.to_str()
                .ok_or_else(|| std::io::Error::other("non-Unicode staging path"))?,
        )?;
        fs::rename(pending, session.join("current-bin"))?;
        Ok(())
    };
    publish().map_err(|e| format!("staging failed; previous selection retained: {e}"))?;
    Ok(bin)
}

fn run(options: Options) -> Result<i32, String> {
    // Cargo on Windows can place the whole interactive session in a job that
    // forbids breakaway. Refuse before expensive builds or session files, rather
    // than opening a shell whose agents cannot later be started/replaced.
    #[cfg(windows)]
    if !options.build_only {
        restart::check_shell_lifetime().map_err(|error| format!(
            "Windows development shell could not open. Run .\\dev.ps1 directly from your normal PowerShell terminal instead of cargo dev.\nDetails: {error}"
        ))?;
    } else if options.restart.is_some() {
        restart::check_shell_lifetime()?;
    }
    // Cargo supplies an absolute manifest directory. Taking parents avoids
    // verbatim-path conversion and preserves Windows UNC paths for shell startup.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("development tool is outside its workspace")?
        .to_owned();
    let session = session::open(&repo, options.session, options.build_only)?;
    let _update_lock = if options.update {
        Some(session::update_lock(&session)?)
    } else {
        None
    };
    let bin = stage(&session, &build(&repo, options.release)?)?;
    println!("Build ready: {}", bin.display());
    if options.pair {
        let root = options
            .server_root
            .as_ref()
            .cloned()
            .or_else(|| std::env::var_os("VESTA_SERVER_ROOT").map(PathBuf::from))
            .unwrap_or_else(|| default_server_root(&repo));
        let status = Command::new(bin.join(executable("vesta")))
            .args(["pair", "dev-server", "--install-root"])
            .arg(root)
            .args(["--role", "owner"])
            .status()
            .map_err(|e| e.to_string())?;
        return Ok(status.code().unwrap_or(1));
    }
    if let Some(target) = options.restart.as_deref() {
        let root = options
            .server_root
            .as_ref()
            .cloned()
            .or_else(|| std::env::var_os("VESTA_SERVER_ROOT").map(PathBuf::from))
            .or_else(|| options.update.then(|| default_server_root(&repo)))
            .ok_or("restart requires VESTA_SERVER_ROOT or --server-root")?;
        let create_default = options.update
            && target == "dev-server"
            && options.server_root.is_none()
            && std::env::var_os("VESTA_SERVER_ROOT").is_none();
        let created = if create_default {
            bootstrap::ensure(&bin, &root)?
        } else {
            false
        };
        println!("Server data: {}", root.display());
        if created {
            restart::start_new(&bin, &session, &root, options.local_only)?;
            bootstrap::finish(&root)?;
        } else {
            restart::run(
                &bin,
                &session,
                &root,
                target,
                options.local_only,
                options.update.then_some(false),
            )?;
        }
        if created && options.local_only != Some(true) {
            let status = Command::new(bin.join(executable("vesta")))
                .args(["pair", "dev-server", "--install-root"])
                .arg(&root)
                .args(["--role", "owner"])
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("dev server is online; pairing failed. Run ./dev.ps1 shell, then vesta pair dev-server --role owner".into());
            }
        }
    }
    if options.build_only {
        return Ok(0);
    }
    let server_root = options
        .server_root
        .or_else(|| std::env::var_os("VESTA_SERVER_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| default_server_root(&repo));
    let server_root = if server_root.is_absolute() {
        server_root
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(server_root)
    };
    let mut shell = if cfg!(windows) {
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoLogo", "-NoProfile", "-NoExit", "-File"])
            .arg(repo.join("scripts/dev-session.ps1"));
        command
    } else {
        let mut command = Command::new("bash");
        command
            .args(["--noprofile", "--rcfile"])
            .arg(repo.join("scripts/dev-session.sh"))
            .arg("-i");
        command
    };
    // Windows broadcasts Ctrl+C to console-attached ancestors too. Let the shell
    // and its foreground command handle it without terminating their waiting parent.
    #[cfg(windows)]
    let _console = console::KeepParentAlive::install().map_err(|e| e.to_string())?;
    let status = shell
        .env("VESTA_DEV_REPO", &repo)
        .env("VESTA_DEV_SESSION", &session)
        .env(
            "VESTA_DEV_TOOL",
            std::env::current_exe().map_err(|e| e.to_string())?,
        )
        .env("VESTA_DEV_RELEASE", if options.release { "1" } else { "0" })
        .env("VESTA_SERVER_ROOT", &server_root)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(status.code().unwrap_or(1))
}

fn default_server_root(repo: &Path) -> PathBuf {
    repo.join(".dev").join("servers")
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    match options(args).and_then(run) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("vesta-dev: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_server_data_is_checkout_local_and_distinct_from_build_staging() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(default_server_root(repo), repo.join(".dev/servers"));
    }

    #[test]
    fn restart_is_explicit_and_requires_a_development_session() {
        let parse = |args: &[&str]| options(args.iter().map(OsString::from));
        for args in [
            vec!["--restart", "example"],
            vec!["build", "--restart", "example"],
            vec!["build", "--session", "session", "--restart"],
            vec!["build", "--session", "session", "--local-only"],
            vec![
                "build",
                "--session",
                "session",
                "--restart",
                "example",
                "--restart",
                "other",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        let selected = parse(&[
            "build",
            "--session",
            "session",
            "--restart",
            "example",
            "--configured-listener",
        ])
        .unwrap();
        assert_eq!(selected.restart.as_deref(), Some("example"));
        assert_eq!(selected.local_only, Some(false));
    }

    #[test]
    fn single_command_updates_default_or_selected_server_without_an_interactive_session() {
        let parse = |args: &[&str]| options(args.iter().map(OsString::from));
        for command in ["update", "restart"] {
            let options = parse(&[command]).unwrap();
            assert!(options.update && options.build_only && options.session.is_none());
            assert_eq!(options.restart.as_deref(), Some("dev-server"));
            let options = parse(&[
                command,
                "another-server",
                "--server-root",
                "registry",
                "--release",
            ])
            .unwrap();
            assert_eq!(options.restart.as_deref(), Some("another-server"));
            assert!(options.release);
        }
        assert!(parse(&["update", "one", "two"]).is_err());
        assert!(parse(&["shell"]).is_ok_and(|o| !o.build_only && !o.update));
        assert!(parse(&["pair"]).is_ok_and(|o| o.build_only && o.pair && !o.update));
        assert!(parse(&["update", "--local-only", "--configured-listener"]).is_err());
    }

    #[test]
    fn only_executable_binary_artifacts_are_selected() {
        let value = serde_json::json!({"reason":"compiler-artifact", "target":{"name":"vesta", "kind":["bin"]}, "executable":"C:/path with spaces/vesta.exe"});
        assert_eq!(
            artifact(&value).unwrap().1,
            PathBuf::from("C:/path with spaces/vesta.exe")
        );
        let mut library = value.clone();
        library["target"]["kind"] = serde_json::json!(["lib"]);
        assert!(artifact(&library).is_none());
        let mut unrelated = value;
        unrelated["target"]["name"] = "vesta-dev".into();
        assert!(artifact(&unrelated).is_none());
    }

    #[test]
    fn incomplete_stage_retains_selected_build_and_existing_executable() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, "first").unwrap();
        let mut artifacts: BTreeMap<_, _> = BINARIES
            .into_iter()
            .map(|name| (name.to_owned(), source.clone()))
            .collect();
        let old = stage(root.path(), &artifacts).unwrap();
        let handle = fs::File::open(old.join(executable("vesta-agent"))).unwrap();
        artifacts.insert("vesta-setup".into(), root.path().join("missing"));
        assert!(stage(root.path(), &artifacts).is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("current-bin")).unwrap(),
            old.to_str().unwrap()
        );
        assert_eq!(
            fs::read_to_string(old.join(executable("vesta-agent"))).unwrap(),
            "first"
        );
        drop(handle);
        artifacts.insert("vesta-setup".into(), source.clone());
        fs::write(source, "second").unwrap();
        let new = stage(root.path(), &artifacts).unwrap();
        assert_ne!(old, new);
        assert_eq!(
            fs::read_to_string(new.join(executable("vesta-agent"))).unwrap(),
            "second"
        );
        assert_eq!(stage(root.path(), &artifacts).unwrap(), new);
        assert_eq!(
            fs::read_to_string(old.join(executable("vesta-agent"))).unwrap(),
            "first"
        );
    }
}
