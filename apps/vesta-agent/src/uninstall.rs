//! OS-local uninstall. A reviewed name never authorizes a changed registry target.
use crate::{
    control,
    local_files::{self, Result},
    servers,
    uninstall_fs::Tree,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};
use vesta_control::config::HostConfiguration;
use vesta_state::{RunnerLock, StateDirectory};

pub const USAGE: &str = "Uninstall\n\n  vesta uninstall <server_id|name> [--install-root <registry>] [--plan]\n  vesta manage <server_id|name> uninstall\n  vesta uninstall --root <Vesta installation> [--servers-root <registry>] [--plan]\n\nInteractive removal requires the exact server name and final UNINSTALL phrase.\nWhole-Vesta removal asks whether to retain or permanently delete server data.\n--plan prints the selected paths without stopping or removing anything.\nThere is no --yes/--force confirmation bypass; external data and Java are preserved.";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub server: Option<String>,
    pub root: Option<PathBuf>,
    pub registry: Option<PathBuf>,
    pub plan: bool,
}
pub fn parse(args: &[OsString]) -> std::result::Result<Request, ()> {
    let mut request = Request {
        server: None,
        root: None,
        registry: None,
        plan: false,
    };
    let mut at = 1;
    if args
        .get(at)
        .is_some_and(|a| !a.to_string_lossy().starts_with('-'))
    {
        request.server = Some(args[at].to_str().ok_or(())?.to_owned());
        at += 1;
    }
    while at < args.len() {
        match args[at].to_str() {
            Some("--plan") if !request.plan => request.plan = true,
            Some("--root") if request.root.is_none() => {
                at += 1;
                request.root = Some(PathBuf::from(
                    args.get(at)
                        .filter(|a| !a.is_empty() && !a.to_string_lossy().starts_with('-'))
                        .ok_or(())?,
                ));
            }
            Some("--install-root" | "--servers-root") if request.registry.is_none() => {
                at += 1;
                request.registry = Some(PathBuf::from(
                    args.get(at)
                        .filter(|a| !a.is_empty() && !a.to_string_lossy().starts_with('-'))
                        .ok_or(())?,
                ));
            }
            _ => return Err(()),
        }
        at += 1;
    }
    if request.server.is_some() && request.root.is_some() {
        return Err(());
    }
    Ok(request)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Target {
    name: String,
    id: String,
    directory: PathBuf,
    config_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Plan {
    installation: Option<PathBuf>,
    registry: PathBuf,
    targets: Vec<Target>,
    setup_sha256: Option<String>,
    preservation: Option<PathBuf>,
}
fn hash(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        _ => Ok(Some(format!(
            "{:x}",
            Sha256::digest(local_files::read(path, 1024 * 1024)?)
        ))),
    }
}
fn target(name: String, id: String, directory: PathBuf) -> Result<Target> {
    local_files::guard(&directory)?;
    Ok(Target {
        name,
        id,
        config_sha256: hash(&directory.join("host.toml"))?,
        directory,
    })
}
fn within(path: &Path, root: &Path) -> bool {
    let path = vesta_setup::paths::comparable(path);
    let root = vesta_setup::paths::comparable(root);
    path == root || path.starts_with(&(root + "/"))
}
fn relocated(path: &Path, root: &Path, destination: &Path) -> Option<PathBuf> {
    within(path, root).then(|| {
        let relative: PathBuf = path.components().skip(root.components().count()).collect();
        destination.join(relative)
    })
}
fn plan(request: &Request, base: &Path) -> Result<Plan> {
    if let Some(selected) = &request.server {
        let registry = servers::root(request.registry.as_deref(), base)?;
        vesta_setup::engine::guard(&registry).map_err(|_| "uninstall_registry_refused")?;
        let (name, id, server) = servers::select(&registry, selected)?;
        return Ok(Plan {
            installation: None,
            registry,
            targets: vec![target(name, id, server.directory)?],
            setup_sha256: None,
            preservation: None,
        });
    }
    let root = if let Some(root) = &request.root {
        local_files::absolute(root, base)?
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().and_then(Path::parent).map(Path::to_owned))
            .ok_or("uninstall_root_required")?
    };
    let receipt = vesta_setup::engine::read_receipt(&root)
        .map_err(|_| "uninstall_valid_setup_receipt_required")?;
    let registry = local_files::absolute(
        request.registry.as_deref().unwrap_or(&root.join("servers")),
        base,
    )?;
    if within(&root, &registry) {
        return Err("uninstall_registry_must_not_contain_installation");
    }
    vesta_setup::engine::guard(&registry).map_err(|_| "uninstall_registry_refused")?;
    if request.registry.is_some() && !registry.exists() {
        return Err("uninstall_selected_registry_missing");
    }
    if registry.exists() {
        let empty_default = registry == root.join("servers")
            && fs::read_dir(&registry)
                .map_err(|_| "uninstall_registry_unavailable")?
                .next()
                .is_none();
        if !empty_default
            && (!registry.join("names").is_dir() || !registry.join("instances").is_dir())
        {
            return Err("uninstall_dedicated_registry_required");
        }
        for entry in fs::read_dir(&registry).map_err(|_| "uninstall_registry_unavailable")? {
            let name = entry
                .map_err(|_| "uninstall_registry_unavailable")?
                .file_name();
            if !matches!(
                name.to_str(),
                Some(
                    "names"
                        | "instances"
                        | "recoverable"
                        | ".staging"
                        | "registry.lock"
                        | "uninstall-pending"
                )
            ) {
                return Err("uninstall_unknown_registry_entries_refused");
            }
        }
    }
    let mut targets = Vec::new();
    for listed in servers::list(&registry)? {
        let (_, id, server) = servers::select(&registry, &listed.name)?;
        targets.push(target(listed.name, id, server.directory)?);
    }
    // Interrupted/unregistered installs can still have a game or supervisor.
    let instances = registry.join("instances");
    local_files::guard(&instances)?;
    if instances.exists() {
        for (index, entry) in fs::read_dir(&instances)
            .map_err(|_| "uninstall_registry_unavailable")?
            .enumerate()
        {
            if index >= 8192 {
                return Err("uninstall_registry_limit_reached");
            }
            let entry = entry.map_err(|_| "uninstall_registry_unavailable")?;
            let id = entry
                .file_name()
                .into_string()
                .map_err(|_| "uninstall_registry_invalid")?;
            if id.ends_with(".operator-lock") {
                continue;
            }
            if !servers::valid_instance_id(&id) {
                return Err("uninstall_unknown_instance_refused");
            }
            if !targets.iter().any(|t| t.id == id) {
                targets.push(target(format!("unregistered {id}"), id, entry.path())?);
            }
        }
    }
    let recovery = registry.join("recoverable");
    local_files::guard(&recovery)?;
    if recovery.exists() {
        for (index, entry) in fs::read_dir(&recovery)
            .map_err(|_| "uninstall_registry_unavailable")?
            .enumerate()
        {
            if index >= 4096 {
                return Err("uninstall_registry_limit_reached");
            }
            let entry = entry.map_err(|_| "uninstall_registry_unavailable")?;
            let recovery_id = entry
                .file_name()
                .into_string()
                .map_err(|_| "uninstall_unknown_recovery_refused")?;
            if uuid::Uuid::parse_str(&recovery_id).is_err() {
                return Err("uninstall_unknown_recovery_refused");
            }
            let directory = entry.path().join("server");
            local_files::guard(&directory)?;
            if directory.exists() {
                targets.push(target(
                    format!("recovery {}", entry.file_name().to_string_lossy()),
                    format!("recovery:{}", entry.file_name().to_string_lossy()),
                    directory,
                )?);
            }
        }
    }
    targets.sort_by(|a, b| a.id.cmp(&b.id));
    let preservation = root
        .parent()
        .map(|parent| parent.join(format!("vesta-preserved-{}", receipt.installation_id)));
    Ok(Plan {
        setup_sha256: hash(&root.join("private/setup.json"))?,
        installation: Some(root),
        registry,
        targets,
        preservation,
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum DataChoice {
    Keep,
    Remove,
}
fn line(input: &mut impl BufRead, output: &mut impl Write, prompt: &str) -> Result<String> {
    write!(output, "{prompt}")
        .and_then(|()| output.flush())
        .map_err(|_| "uninstall_terminal_unavailable")?;
    let mut value = String::new();
    let read = input
        .take(4097)
        .read_line(&mut value)
        .map_err(|_| "uninstall_terminal_unavailable")?;
    if read == 0 || value.len() > 4096 || value.contains('\u{1b}') || value.contains('\u{3}') {
        return Err("uninstall_cancelled");
    }
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}
fn confirm(plan: &Plan, input: &mut impl BufRead, output: &mut impl Write) -> Result<DataChoice> {
    let subject = if plan.installation.is_some() {
        "VESTA"
    } else {
        &plan.targets[0].name
    };
    writeln!(output, "\nVESTA  /  Uninstall {subject}\n")
        .map_err(|_| "uninstall_terminal_unavailable")?;
    if !matches!(
        line(input, output, "Continue with uninstallation? [y/N] ")?.as_str(),
        "y" | "Y" | "yes"
    ) {
        return Err("uninstall_cancelled");
    }
    if plan.installation.is_none()
        && line(
            input,
            output,
            &format!("Type the server name '{subject}': "),
        )? != subject
    {
        return Err("uninstall_name_confirmation_mismatch");
    }
    let remove = line(
        input,
        output,
        if plan.installation.is_some() {
            "Also permanently remove all servers in this registry, including worlds and recovery copies? [y/N] "
        } else {
            "Permanently delete its files, worlds and backups? [y/N; default keeps a recovery copy] "
        },
    )?;
    let choice = if matches!(remove.as_str(), "y" | "Y" | "yes") {
        DataChoice::Remove
    } else if matches!(remove.as_str(), "" | "n" | "N" | "no") {
        DataChoice::Keep
    } else {
        return Err("uninstall_invalid_choice");
    };
    review(plan, choice, output)?;
    if line(
        input,
        output,
        &format!("Final confirmation: type 'UNINSTALL {subject}': "),
    )? != format!("UNINSTALL {subject}")
    {
        return Err("uninstall_final_confirmation_mismatch");
    }
    Ok(choice)
}
fn review(plan: &Plan, choice: DataChoice, out: &mut impl Write) -> Result<()> {
    let result = (|| -> io::Result<()> {
        writeln!(
            out,
            "\nFinal review\n  Running managed servers will be stopped gracefully. No force-kill."
        )?;
        for t in &plan.targets {
            writeln!(
                out,
                "  Server: {} ({})\n    {}",
                t.name,
                t.id,
                t.directory.display()
            )?;
        }
        if let Some(root) = &plan.installation {
            writeln!(
                out,
                "  Remove Vesta binaries and its owned startup registration at {}",
                root.display()
            )?;
            writeln!(out, "  Server registry: {}", plan.registry.display())?;
            match choice {
                DataChoice::Remove => writeln!(
                    out,
                    "  PERMANENTLY DELETE this installation and registry, including unregistered/recovery copies. This cannot be undone."
                )?,
                DataChoice::Keep => {
                    writeln!(
                        out,
                        "  KEEP server data. Any data within the installation moves to {}",
                        plan.preservation.as_ref().unwrap().display()
                    )?;
                    writeln!(
                        out,
                        "  External registry/data remain in place. Preserved absolute paths may need reconfiguration on reinstall."
                    )?;
                    if let Some(location) =
                        relocated(&plan.registry, root, plan.preservation.as_ref().unwrap())
                    {
                        writeln!(out, "  This registry moves to {}", location.display())?;
                    }
                }
            }
            writeln!(
                out,
                "  External Java, game directories and backup repositories are never recursively deleted."
            )?;
        } else {
            writeln!(
                out,
                "  {}",
                if choice == DataChoice::Remove {
                    "PERMANENTLY DELETE this server directory after unregistering it. This cannot be undone."
                } else {
                    "Unregister this server and KEEP all its files in the registry's recoverable directory."
                }
            )?;
            writeln!(out, "  External configured data/Java stay untouched.")?;
        }
        Ok(())
    })();
    result.map_err(|_| "uninstall_terminal_unavailable")
}

pub fn check_start(source: &Path) -> Result<()> {
    if let Some(directory) = source.parent() {
        for root in directory.ancestors() {
            for marker in [
                root.join("uninstall-pending"),
                root.join("private/uninstall-pending"),
            ] {
                local_files::guard(&marker)?;
                if marker.exists() {
                    return Err("uninstall_pending_start_refused");
                }
            }
        }
    }
    Ok(())
}

async fn stop_config(config: &Path) -> Result<()> {
    if !config.exists() {
        return Ok(());
    }
    let base = config.parent().ok_or("uninstall_config_invalid")?;
    let configuration =
        HostConfiguration::load_file(config, base).map_err(|_| "uninstall_config_unreadable")?;
    let name = servers::control_name(config)?;
    match control::request(configuration.state(), name, control::Op::Shutdown).await {
        Ok(()) | Err("local_agent_not_running_start_serve_first") => {}
        Err(code) => return Err(code),
    }
    let state = StateDirectory::new(configuration.state(), Some(configuration.game()))
        .map_err(|_| "uninstall_state_refused")?;
    if state.path().exists() {
        let _ = crate::supervision::release(&state);
    }
    Ok(())
}
fn quiescent(directory: &Path) -> Result<Vec<RunnerLock>> {
    crate::sessions::require_stopped(directory)?;
    let mut states = vec![(directory.join("state"), directory.join("game"))];
    let source = directory.join("host.toml");
    if source.exists() {
        let c = HostConfiguration::load_file(&source, directory)
            .map_err(|_| "uninstall_config_unreadable")?;
        if !states.iter().any(|(s, _)| s == c.state()) {
            states.push((c.state().to_owned(), c.game().to_owned()));
        }
    }
    let mut guards = Vec::new();
    for (path, game) in states {
        if !path.exists() {
            continue;
        }
        let state =
            StateDirectory::new(&path, Some(&game)).map_err(|_| "uninstall_state_refused")?;
        if vesta_supervisor::instance_busy(&state).map_err(|_| "uninstall_supervision_unknown")? {
            return Err("uninstall_supervisor_or_game_still_running");
        }
        guards.push(
            state
                .acquire_runner_lock()
                .map_err(|_| "uninstall_agent_still_running")?,
        );
    }
    Ok(guards)
}
fn wait_quiescent(directory: &Path) -> Result<Vec<RunnerLock>> {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        match quiescent(directory) {
            Ok(guards) => return Ok(guards),
            Err(code) if Instant::now() >= until => return Err(code),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

pub async fn run(request: Request, base: &Path) -> ExitCode {
    match run_inner(request, base).await {
        Ok(()) => ExitCode::SUCCESS,
        Err("uninstall_handed_off") => ExitCode::from(75),
        Err("uninstall_cancelled") => {
            eprintln!("Uninstall cancelled; no removal performed.");
            ExitCode::from(130)
        }
        Err(code) => {
            eprintln!("Uninstall stopped: {code}. Remaining files were not reported as removed.");
            ExitCode::FAILURE
        }
    }
}
async fn run_inner(request: Request, base: &Path) -> Result<()> {
    let reviewed = plan(&request, base)?;
    if request.plan {
        println!(
            "{}",
            serde_json::to_string_pretty(&reviewed).map_err(|_| "uninstall_plan_unavailable")?
        );
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("uninstall_interactive_terminal_required_no_yes_bypass");
    }
    let choice = confirm(&reviewed, &mut io::stdin().lock(), &mut io::stdout().lock())?;
    let cwd = std::env::current_dir().map_err(|_| "uninstall_working_directory_unavailable")?;
    let destination = outside_removal_cwd(&reviewed, &cwd)?;
    if destination != cwd {
        std::env::set_current_dir(destination)
            .map_err(|_| "uninstall_working_directory_unavailable")?;
    }
    #[cfg(windows)]
    {
        let setup_parent = std::env::var("VESTA_UNINSTALL_SETUP_PID")
            .ok()
            .map(|value| value.parse::<u32>().map_err(|_| "uninstall_helper_invalid"))
            .transpose()?
            .filter(|pid| *pid != 0 && *pid != std::process::id());
        if reviewed.installation.as_ref().is_some_and(|root| {
            setup_parent.is_some() || std::env::current_exe().is_ok_and(|exe| within(&exe, root))
        }) {
            stage_self(reviewed, choice, request, setup_parent)?;
            return Err("uninstall_handed_off");
        }
    }
    execute(reviewed, choice, &request, base).await
}

// Windows retains the process working directory. Leave every selected tree,
// including an external registry, before moving or removing any of them.
fn outside_removal_cwd(reviewed: &Plan, cwd: &Path) -> Result<PathBuf> {
    let roots: Vec<&Path> = if let Some(root) = &reviewed.installation {
        vec![root, &reviewed.registry]
    } else {
        reviewed
            .targets
            .iter()
            .map(|t| t.directory.as_path())
            .collect()
    };
    let mut destination = cwd;
    while roots.iter().any(|root| within(destination, root)) {
        destination = destination.parent().ok_or("uninstall_path_refused")?;
    }
    Ok(destination.to_owned())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    token: String,
    parent: u32,
    #[serde(default)]
    setup_parent: Option<u32>,
    plan: Plan,
    choice: DataChoice,
    request: Request,
}

#[cfg(windows)]
fn stage_self(
    plan: Plan,
    choice: DataChoice,
    mut request: Request,
    setup_parent: Option<u32>,
) -> Result<()> {
    let directory =
        std::env::temp_dir().join(format!("vesta-uninstall-{}", uuid::Uuid::new_v4().simple()));
    let private =
        StateDirectory::new(&directory, None).map_err(|_| "uninstall_helper_unavailable")?;
    private
        .prepare()
        .map_err(|_| "uninstall_helper_unavailable")?;
    let executable = directory.join("vesta.exe");
    fs::copy(
        std::env::current_exe().map_err(|_| "uninstall_helper_unavailable")?,
        &executable,
    )
    .map_err(|_| "uninstall_helper_unavailable")?;
    private
        .tighten_file(&executable)
        .map_err(|_| "uninstall_helper_unavailable")?;
    request.root = plan.installation.clone();
    request.registry = Some(plan.registry.clone());
    let token = uuid::Uuid::new_v4().simple().to_string();
    let job = Job {
        token: token.clone(),
        parent: std::process::id(),
        setup_parent,
        plan,
        choice,
        request,
    };
    let path = directory.join("job.json");
    local_files::write_new(
        &path,
        &serde_json::to_vec(&job).map_err(|_| "uninstall_helper_unavailable")?,
    )?;
    private
        .tighten_file(&path)
        .map_err(|_| "uninstall_helper_unavailable")?;
    std::process::Command::new(executable)
        .arg("uninstall-execute")
        .arg(&path)
        .arg(token)
        .current_dir(&directory)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "uninstall_helper_unavailable")?;
    println!(
        "Uninstallation handed to an external helper so Windows can remove this running executable. Completion receipt: {}",
        directory.join("result.json").display()
    );
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn wait_parent(pid: u32) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, WaitForSingleObject},
    };
    // SAFETY: only request synchronization permission. No process mutation,
    // termination, pointers or transferred ownership. A reused PID can delay the
    // helper but cannot make it delete while its original parent is still alive.
    unsafe {
        let handle = OpenProcess(0x0010_0000, 0, pid);
        if handle.is_null() {
            return if GetLastError() == ERROR_INVALID_PARAMETER {
                Ok(())
            } else {
                Err("uninstall_parent_wait_failed")
            };
        }
        let result = WaitForSingleObject(handle, 15_000);
        CloseHandle(handle);
        if result != WAIT_OBJECT_0 {
            return Err("uninstall_parent_still_running");
        }
    }
    Ok(())
}

pub async fn run_job(path: PathBuf, token: String) -> ExitCode {
    let outcome = async {
        let directory = path.parent().ok_or("uninstall_helper_invalid")?;
        let folder = directory
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("uninstall_helper_invalid")?;
        if path.file_name() != Some(std::ffi::OsStr::new("job.json"))
            || !folder
                .strip_prefix("vesta-uninstall-")
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("uninstall_helper_invalid");
        }
        if std::env::current_exe()
            .map_err(|_| "uninstall_helper_invalid")?
            .parent()
            != Some(directory)
        {
            return Err("uninstall_helper_invalid");
        }
        let job: Job = serde_json::from_slice(&local_files::read(&path, 1024 * 1024)?)
            .map_err(|_| "uninstall_helper_invalid")?;
        if token.len() != 32
            || token != job.token
            || job.parent == std::process::id()
            || job.setup_parent == Some(std::process::id())
            || job.request.plan
        {
            return Err("uninstall_helper_invalid");
        }
        let result = async {
            #[cfg(windows)]
            {
                wait_parent(job.parent)?;
                // Installed setup also holds an executable inside the removal
                // tree. It exits after the CLI reports the handoff (status 75).
                if let Some(pid) = job.setup_parent {
                    wait_parent(pid)?;
                }
            }
            let base = std::env::current_dir().map_err(|_| "uninstall_helper_invalid")?;
            execute(job.plan, job.choice, &job.request, &base).await
        }
        .await;
        let receipt =
            serde_json::json!({"complete":result.is_ok(),"error":result.as_ref().err().copied()});
        local_files::write_new(
            &directory.join("result.json"),
            &serde_json::to_vec(&receipt).map_err(|_| "uninstall_helper_invalid")?,
        )?;
        result
    }
    .await;
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("Uninstall helper stopped: {code}");
            ExitCode::FAILURE
        }
    }
}

fn operation_guard(path: &Path) -> Result<std::fs::File> {
    // Outside every removal tree: retain exclusion through handle release and
    // final unlink/rename. Never unlink a stable advisory lock's identity.
    let key = format!(
        "{:x}",
        Sha256::digest(vesta_setup::paths::comparable(path).as_bytes())
    );
    let root = std::env::temp_dir().join(format!("vesta-uninstall-lock-{key}"));
    StateDirectory::new(&root, None)
        .and_then(|d| d.prepare())
        .map_err(|_| "uninstall_operation_lock_unavailable")?;
    local_files::lock_for_uninstall(&root.join("operation.lock"))
}

async fn execute(reviewed: Plan, choice: DataChoice, request: &Request, base: &Path) -> Result<()> {
    let _registry_operation = operation_guard(&reviewed.registry)?;
    let _installation_operation = reviewed
        .installation
        .as_ref()
        .map(|root| operation_guard(root))
        .transpose()?;
    if reviewed.installation.is_none() {
        check_start(&reviewed.registry.join("registry.lock"))?;
    }
    if let Some(root) = &reviewed.installation {
        if choice == DataChoice::Keep && reviewed.preservation.as_ref().unwrap().exists() {
            return Err("uninstall_preservation_destination_exists");
        }
        if !reviewed.registry.exists() && reviewed.registry == root.join("servers") {
            local_files::guard(&reviewed.registry)?;
            fs::create_dir(&reviewed.registry).map_err(|_| "uninstall_registry_unavailable")?;
        }
        if reviewed.registry == root.join("servers") && !reviewed.registry.join("names").exists() {
            for name in ["names", "instances", "recoverable"] {
                let path = reviewed.registry.join(name);
                local_files::guard(&path)?;
                if !path.exists() {
                    fs::create_dir(path).map_err(|_| "uninstall_registry_unavailable")?;
                }
            }
        }
    }
    let registry_guard = if reviewed.registry.exists() {
        Some(local_files::lock_for_uninstall(
            &reviewed.registry.join("registry.lock"),
        )?)
    } else {
        None
    };
    if plan(request, base)? != reviewed {
        return Err("uninstall_target_changed_review_again");
    }
    if reviewed.installation.is_none() {
        let target = &reviewed.targets[0];
        stop_config(&target.directory.join("host.toml")).await?;
        drop(wait_quiescent(&target.directory)?);
        let recovered = servers::delete_guarded(&reviewed.registry, &target.name, &target.name)?;
        if choice == DataChoice::Remove {
            Tree::scan(&recovered)?.remove()?;
            println!(
                "Server {} uninstalled; managed files removed. External data untouched.",
                target.name
            );
        } else {
            println!(
                "Server {} uninstalled; data preserved at {}",
                target.name,
                recovered.display()
            );
        }
        return Ok(());
    }
    let root = reviewed.installation.as_ref().unwrap();
    let mut receipt =
        vesta_setup::engine::read_receipt(root).map_err(|_| "uninstall_setup_changed")?;
    let setup_guard = local_files::lock_for_uninstall(&root.join("private/setup.lock"))?;
    let marker = root.join("private/uninstall-pending");
    if !marker.exists() {
        local_files::write_new(&marker, receipt.installation_id.as_bytes())?;
    } else if local_files::read(&marker, 64)? != receipt.installation_id.as_bytes() {
        return Err("uninstall_marker_changed");
    }
    let registry_marker = reviewed.registry.join("uninstall-pending");
    if reviewed.registry.exists() {
        if !registry_marker.exists() {
            local_files::write_new(&registry_marker, receipt.installation_id.as_bytes())?;
        } else if local_files::read(&registry_marker, 64)? != receipt.installation_id.as_bytes() {
            return Err("uninstall_registry_marker_changed");
        }
        #[cfg(unix)]
        fs::File::open(&reviewed.registry)
            .and_then(|d| d.sync_all())
            .map_err(|_| "uninstall_marker_sync_failed")?;
    }
    for target in &reviewed.targets {
        stop_config(&target.directory.join("host.toml")).await?;
    }
    stop_config(&root.join("host.toml")).await?;
    // Pending setup host stops cooperatively; activated host exits after its agent.
    let _ = vesta_setup::host::stop(root);
    let until = Instant::now() + Duration::from_secs(12);
    let host_guard = loop {
        match local_files::lock_for_uninstall(&root.join("private/host.lock")) {
            Ok(guard) => break guard,
            Err(code) if Instant::now() >= until => return Err(code),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let mut leases = Vec::new();
    let mut runners = Vec::new();
    for target in &reviewed.targets {
        if let Some(lease) = servers::managed_lease(&target.directory) {
            leases.push(local_files::lock_for_uninstall(&lease)?);
        }
        runners.extend(wait_quiescent(&target.directory)?);
    }
    runners.extend(wait_quiescent(root)?);
    let executable = root
        .join("bin")
        .join(format!("vesta-setup{}", std::env::consts::EXE_SUFFIX));
    if receipt.phase != "uninstalling"
        && (receipt.startup_registered
            || receipt.answers.startup == vesta_setup::engine::Startup::Automatic)
    {
        vesta_setup::platform::disable_startup(root, &executable, &receipt.installation_id)
            .map_err(|_| "uninstall_startup_removal_failed")?;
    }
    receipt.startup_registered = false;
    receipt.phase = "uninstalling".into();
    vesta_setup::engine::write_private(
        root,
        "setup.json",
        &serde_json::to_vec_pretty(&receipt).map_err(|_| "uninstall_journal_failed")?,
    )
    .map_err(|_| "uninstall_journal_failed")?;
    drop(runners);
    drop(leases);
    drop(host_guard);
    drop(setup_guard);
    if choice == DataChoice::Remove {
        if reviewed.registry.exists() && !within(&reviewed.registry, root) {
            drop(registry_guard);
            Tree::scan(&reviewed.registry)?.remove()?;
        } else {
            drop(registry_guard);
        }
        Tree::scan(root)?.remove()?;
        println!("Vesta and managed registry uninstalled. External data/Java were preserved.");
    } else {
        if root.join("bin").exists() {
            Tree::scan(&root.join("bin"))?.remove()?;
        }
        // Keep all remaining host/server state together, outside the removed
        // installation. Never guess which custom server files are disposable.
        let preservation = reviewed.preservation.as_ref().unwrap();
        local_files::guard(preservation)?;
        if preservation.exists() {
            return Err("uninstall_preservation_destination_exists");
        }
        drop(registry_guard);
        fs::rename(root, preservation).map_err(|_| "uninstall_preservation_move_failed")?;
        if !within(&reviewed.registry, root) && registry_marker.exists() {
            local_files::guard(&registry_marker)?;
            if local_files::read(&registry_marker, 64)? != receipt.installation_id.as_bytes() {
                return Err("uninstall_registry_marker_changed");
            }
            fs::remove_file(&registry_marker)
                .map_err(|_| "uninstall_registry_marker_cleanup_failed")?;
        }
        let registry_location = relocated(&reviewed.registry, root, preservation)
            .unwrap_or_else(|| reviewed.registry.clone());
        println!(
            "Vesta uninstalled. Server data/state preserved at {}. Registry: {}",
            preservation.display(),
            registry_location.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry(root: &Path) -> (PathBuf, String) {
        let id = format!("srv_{}", uuid::Uuid::new_v4().simple());
        for path in [
            root.join("names"),
            root.join("recoverable"),
            root.join("instances").join(&id).join("game/world"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let instance = root.join("instances").join(&id);
        fs::write(instance.join("game/world/keep"), b"owned fixture world").unwrap();
        fs::write(instance.join("local-name"), b"owned").unwrap();
        fs::write(
            root.join("names/owned.json"),
            serde_json::to_vec(&serde_json::json!({"schema":1,"instance":id})).unwrap(),
        )
        .unwrap();
        (instance, id)
    }
    fn server_request(root: &Path) -> Request {
        Request {
            server: Some("owned".into()),
            root: None,
            registry: Some(root.to_owned()),
            plan: false,
        }
    }
    fn installation(root: &Path) -> Request {
        fs::create_dir_all(root.join("private")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(
            root.join("bin/vesta-setup.exe"),
            b"inert fixture, never executed",
        )
        .unwrap();
        fs::write(root.join("private/setup.json"), serde_json::to_vec(&serde_json::json!({
            "schema":1,"installation_id":"a".repeat(32),"answers":{"schema":1,"root":root,"existing_server":null,"startup":"manual","port":8443},
            "release_version":"test","release_manifest_sha256":"b".repeat(64),"phase":"ready","tls_sha256":"c".repeat(64),"startup_registered":false
        })).unwrap()).unwrap();
        Request {
            server: None,
            root: Some(root.to_owned()),
            registry: None,
            plan: false,
        }
    }
    #[tokio::test]
    async fn per_server_keep_and_remove_preserve_other_servers_and_external_files() {
        for choice in [DataChoice::Keep, DataChoice::Remove] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("registry");
            let (instance, _) = registry(&root);
            fs::write(temp.path().join("outside"), b"keep").unwrap();
            let request = server_request(&root);
            let reviewed = plan(&request, temp.path()).unwrap();
            execute(reviewed, choice, &request, temp.path())
                .await
                .unwrap();
            assert!(!instance.exists());
            assert!(!root.join("names/owned.json").exists());
            assert_eq!(fs::read(temp.path().join("outside")).unwrap(), b"keep");
            let recovery = fs::read_dir(root.join("recoverable"))
                .unwrap()
                .collect::<Vec<_>>();
            assert_eq!(
                recovery.len(),
                if choice == DataChoice::Keep { 1 } else { 0 }
            );
            if choice == DataChoice::Keep {
                assert!(
                    recovery[0]
                        .as_ref()
                        .unwrap()
                        .path()
                        .join("server/game/world/keep")
                        .is_file()
                );
            }
        }
    }
    #[tokio::test]
    async fn changed_name_binding_and_held_agent_lease_refuse_without_removal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("registry");
        let (instance, id) = registry(&root);
        let request = server_request(&root);
        let reviewed = plan(&request, temp.path()).unwrap();
        let (other, _) = registry(&root);
        assert_eq!(
            execute(reviewed, DataChoice::Remove, &request, temp.path()).await,
            Err("uninstall_target_changed_review_again")
        );
        assert!(instance.join("game/world/keep").is_file());
        assert!(other.join("game/world/keep").is_file());
        fs::write(
            root.join("names/owned.json"),
            serde_json::to_vec(&serde_json::json!({"schema":1,"instance":id})).unwrap(),
        )
        .unwrap();
        let reviewed = plan(&request, temp.path()).unwrap();
        let _lease = local_files::lock(&servers::managed_lease(&instance).unwrap()).unwrap();
        assert!(
            execute(reviewed, DataChoice::Remove, &request, temp.path())
                .await
                .is_err()
        );
        assert!(root.join("names/owned.json").is_file());
        assert!(instance.join("game/world/keep").is_file());
    }
    #[tokio::test]
    async fn whole_installation_keep_and_remove_are_explicit_and_leave_external_data() {
        for choice in [DataChoice::Keep, DataChoice::Remove] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("host");
            let request = installation(&root);
            let (_, id) = registry(&root.join("servers"));
            fs::write(temp.path().join("outside"), b"external data").unwrap();
            let reviewed = plan(&request, temp.path()).unwrap();
            let preservation = reviewed.preservation.clone().unwrap();
            execute(reviewed, choice, &request, temp.path())
                .await
                .unwrap();
            assert!(!root.exists());
            assert_eq!(
                fs::read(temp.path().join("outside")).unwrap(),
                b"external data"
            );
            if choice == DataChoice::Keep {
                assert!(
                    preservation
                        .join("servers/instances")
                        .join(id)
                        .join("game/world/keep")
                        .is_file()
                );
                assert!(!preservation.join("bin").exists());
            } else {
                assert!(!preservation.exists());
            }
        }
    }
    #[tokio::test]
    async fn pending_install_without_a_registry_can_be_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("host");
        let request = installation(&root);
        let reviewed = plan(&request, temp.path()).unwrap();
        execute(reviewed, DataChoice::Remove, &request, temp.path())
            .await
            .unwrap();
        assert!(!root.exists());
    }
    #[test]
    fn whole_uninstall_requires_a_dedicated_registry_and_checks_unproven_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("host");
        let mut request = installation(&root);
        fs::write(temp.path().join("unrelated"), b"keep").unwrap();
        request.registry = Some(temp.path().to_owned());
        assert!(plan(&request, temp.path()).is_err());
        let (instance, _) = registry(&root.join("servers"));
        drop(crate::sessions::Sessions::begin([instance.clone()], false).unwrap());
        assert_eq!(
            quiescent(&instance).unwrap_err(),
            "local_shutdown_unproven_requires_supervisor_recovery"
        );
        assert!(instance.join("game/world/keep").is_file());
    }
    #[test]
    fn whole_confirmation_defaults_to_retaining_servers_and_final_review_names_paths() {
        let mut reviewed = sample();
        reviewed.installation = Some(PathBuf::from("host"));
        reviewed.preservation = Some(PathBuf::from("preserved"));
        let mut output = Vec::new();
        assert_eq!(
            confirm(
                &reviewed,
                &mut io::Cursor::new("yes\n\nUNINSTALL VESTA\n"),
                &mut output
            )
            .unwrap(),
            DataChoice::Keep
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("test-server"));
        assert!(output.contains("KEEP server data"));
        assert!(output.contains("preserved"));
    }
    fn sample() -> Plan {
        Plan {
            installation: None,
            registry: PathBuf::from("registry"),
            targets: vec![Target {
                name: "test-server".into(),
                id: "srv_x".into(),
                directory: PathBuf::from("instance"),
                config_sha256: None,
            }],
            setup_sha256: None,
            preservation: None,
        }
    }
    #[test]
    fn name_and_final_confirmation_are_both_required_and_cancellation_is_default() {
        for input in [
            "\n",
            "n\n",
            "yes\nwrong\n",
            "yes\ntest-server\ny\nyes\n",
            "yes\ntest-server\ny\n",
        ] {
            assert!(confirm(&sample(), &mut io::Cursor::new(input), &mut Vec::new()).is_err());
        }
        assert_eq!(
            confirm(
                &sample(),
                &mut io::Cursor::new("yes\ntest-server\ny\nUNINSTALL test-server\n"),
                &mut Vec::new()
            )
            .unwrap(),
            DataChoice::Remove
        );
        assert_eq!(
            confirm(
                &sample(),
                &mut io::Cursor::new("yes\ntest-server\n\nUNINSTALL test-server\n"),
                &mut Vec::new()
            )
            .unwrap(),
            DataChoice::Keep
        );
    }
    #[test]
    fn maintenance_marker_blocks_normal_deletion_and_offline_mutation_locks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("registry");
        let (instance, _) = registry(&root);
        fs::write(root.join("uninstall-pending"), b"owned uninstall").unwrap();
        assert_eq!(
            servers::delete(&root, "owned", "owned"),
            Err("uninstall_pending_start_refused")
        );
        assert!(local_files::lock(&servers::managed_lease(&instance).unwrap()).is_err());
        assert_eq!(
            check_start(&instance.join("host.toml")),
            Err("uninstall_pending_start_refused")
        );
        assert!(instance.join("game/world/keep").is_file());
        assert!(root.join("names/owned.json").is_file());
    }
    #[test]
    fn no_force_or_yes_can_bypass_interactive_verification() {
        for args in [
            vec!["uninstall", "a", "--yes"],
            vec!["uninstall", "--force"],
            vec!["uninstall", "a", "--root", "b"],
            vec!["uninstall", "--plan", "--plan"],
        ] {
            assert!(parse(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err());
        }
    }
    #[test]
    fn working_directory_leaves_single_server_and_whole_registry_trees() {
        let temp = tempfile::tempdir().unwrap();
        let mut reviewed = sample();
        reviewed.registry = temp.path().join("registry");
        reviewed.targets[0].directory = reviewed.registry.join("instances/server");
        let cwd = reviewed.targets[0].directory.join("game/world");
        assert_eq!(
            outside_removal_cwd(&reviewed, &cwd).unwrap(),
            reviewed.registry.join("instances")
        );
        reviewed.installation = Some(temp.path().join("host"));
        assert_eq!(outside_removal_cwd(&reviewed, &cwd).unwrap(), temp.path());
        let host_cwd = reviewed.installation.as_ref().unwrap().join("bin");
        assert_eq!(
            outside_removal_cwd(&reviewed, &host_cwd).unwrap(),
            temp.path()
        );
        reviewed.registry = reviewed.installation.as_ref().unwrap().join("servers");
        assert_eq!(
            outside_removal_cwd(&reviewed, &reviewed.registry.join("instances")).unwrap(),
            temp.path()
        );
        assert_eq!(
            outside_removal_cwd(&reviewed, temp.path()).unwrap(),
            temp.path()
        );
    }
    #[cfg(windows)]
    #[test]
    fn moved_registry_path_uses_windows_case_and_verbatim_identity() {
        let destination = Path::new(r"C:\preserved");
        for registry in [r"c:\HOST\Servers", r"\\?\C:\Host\Servers"] {
            assert_eq!(
                relocated(Path::new(registry), Path::new(r"C:\Host"), destination),
                Some(destination.join("Servers"))
            );
        }
    }
    #[tokio::test]
    async fn another_uninstaller_cannot_enter_after_internal_handles_are_released() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("registry");
        let (instance, _) = registry(&root);
        let request = server_request(&root);
        let reviewed = plan(&request, temp.path()).unwrap();
        let _guard = operation_guard(&root).unwrap();
        assert!(
            execute(reviewed, DataChoice::Remove, &request, temp.path())
                .await
                .is_err()
        );
        assert!(instance.join("game/world/keep").is_file());
    }
}
