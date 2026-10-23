//! Development-only build staging and shell entrypoint. Never manages processes or server data.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

const BINARIES: [&str; 4] = ["vesta", "vesta-agent", "vesta-supervisor", "vesta-setup"];
#[cfg(windows)]
mod console;
const HELP: &str = "Vesta development shell\n\n  cargo dev [--release] [--server-root <registry>]\n  cargo dev build [--release]\n  cargo run -p vesta-dev -- [same options]\n\nBuilds and stages all four commands. The default opens a child terminal session\nwith temporary PATH and VESTA_SERVER_ROOT. Use vesta-rebuild there; exit returns\nto the original terminal. Build/rebuild never installs, starts or stops a server.\n";

#[derive(Default)]
struct Options {
    build_only: bool,
    release: bool,
    session: Option<PathBuf>,
    server_root: Option<PathBuf>,
}

fn options(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut result = Options::default();
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|v| v == "build") {
        result.build_only = true;
        args.next();
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
            _ => {
                return Err(format!(
                    "invalid development option: {}",
                    arg.to_string_lossy()
                ));
            }
        }
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
    // Cargo supplies an absolute manifest directory. Taking parents avoids
    // verbatim-path conversion and preserves Windows UNC paths for shell startup.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("development tool is outside its workspace")?
        .to_owned();
    let session = match options.session {
        Some(session) => {
            if fs::read(session.join(".vesta-dev-session")).map_err(|e| e.to_string())?
                != repo.as_os_str().as_encoded_bytes()
            {
                return Err("session belongs to another checkout".into());
            }
            session
        }
        None => {
            let session = std::env::temp_dir()
                .join("vesta-dev-builds")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&session).map_err(|e| e.to_string())?;
            fs::write(
                session.join(".vesta-dev-session"),
                repo.as_os_str().as_encoded_bytes(),
            )
            .map_err(|e| e.to_string())?;
            session
        }
    };
    let bin = stage(&session, &build(&repo, options.release)?)?;
    println!("Build ready: {}", bin.display());
    if options.build_only {
        return Ok(0);
    }
    let server_root = options
        .server_root
        .or_else(|| std::env::var_os("VESTA_SERVER_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| std::env::temp_dir().join("vesta-dev-server/servers"));
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

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
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
        assert_eq!(
            fs::read_to_string(old.join(executable("vesta-agent"))).unwrap(),
            "first"
        );
    }
}
