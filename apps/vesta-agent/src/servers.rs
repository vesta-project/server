//! Named local installs. No world erasure, adoption, remote authority or automatic EULA.
use crate::{
    cli::Create,
    local_files::{self, Result},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};
use vesta_server_install::{
    Acknowledgments, Approval, InstallRoots, Receipt, Selection, ServerInstaller,
    SilentProgressReporter, TransportConfig,
};
use vesta_state::{
    StateDatabase, StateDirectory, SystemClock,
    audit::{AuditActor, AuditLog, AuditRecord},
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u8,
    instance: String,
}
pub struct Server {
    pub directory: PathBuf,
    pub config: PathBuf,
}
pub fn valid_instance_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("srv_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Serialize)]
pub struct ListedServer {
    pub id: String,
    pub name: String,
}

/// Enumerate the registry only: no state migrations, sockets, game or directory creation.
pub fn list(root: &Path) -> Result<Vec<ListedServer>> {
    local_files::guard(root)?;
    let names = root.join("names");
    local_files::guard(&names)?;
    let entries = match fs::read_dir(&names) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("local_registry_unavailable"),
    };
    let mut listed = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err("local_registry_limit_reached");
        }
        let entry = entry.map_err(|_| "local_registry_unavailable")?;
        let filename = entry.file_name();
        let name = filename
            .to_str()
            .and_then(|s| s.strip_suffix(".json"))
            .filter(|s| crate::cli::valid_name(s))
            .ok_or("local_registry_invalid")?;
        let (_, id, _) = select(root, name)?;
        listed.push(ListedServer {
            id,
            name: name.into(),
        });
    }
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(listed)
}

/// IDs are stable local installation IDs; names remain convenient aliases.
pub fn select(root: &Path, target: &str) -> Result<(String, String, Server)> {
    local_files::guard(root)?;
    let name = if valid_instance_id(target) {
        let bytes = local_files::read(&root.join("instances").join(target).join("local-name"), 48)?;
        String::from_utf8(bytes).map_err(|_| "local_record_invalid")?
    } else {
        target.to_owned()
    };
    let server = resolve(root, &name)?;
    let id = server
        .directory
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("local_record_invalid")?
        .to_owned();
    if valid_instance_id(target) && target != id {
        return Err("local_server_identity_changed");
    }
    Ok((name, id, server))
}
pub fn root(selected: Option<&Path>, base: &Path) -> Result<PathBuf> {
    let fallback = std::env::var_os("VESTA_SERVER_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("vesta-servers"));
    local_files::absolute(selected.unwrap_or(&fallback), base)
}
fn prepare(root: &Path) -> Result<()> {
    StateDirectory::new(root, None)
        .and_then(|d| d.prepare())
        .map_err(|_| "local_root_refused")?;
    for name in ["names", "instances", "recoverable"] {
        let path = root.join(name);
        local_files::guard(&path)?;
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(_) => return Err("local_root_refused"),
        }
    }
    Ok(())
}
fn record_path(root: &Path, name: &str) -> Result<PathBuf> {
    if !crate::cli::valid_name(name) {
        return Err("local_name_invalid");
    }
    Ok(root.join("names").join(format!("{name}.json")))
}
pub fn resolve(root: &Path, name: &str) -> Result<Server> {
    local_files::guard(root)?;
    let bytes = local_files::read(&record_path(root, name)?, 4096)?;
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| "local_record_invalid")?;
    if record.schema != 1
        || !record.instance.starts_with("srv_")
        || record.instance.len() != 36
        || !record.instance[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("local_record_invalid");
    }
    let directory = root.join("instances").join(record.instance);
    local_files::guard(&directory)?;
    if !directory.is_dir() {
        return Err("local_server_missing");
    }
    Ok(Server {
        config: directory.join("host.toml"),
        directory,
    })
}
pub fn check_port(bind: IpAddr, port: u16) -> Result<()> {
    // Bind-test only: never listen(), accept(), advertise or change host networking.
    if !bind.is_loopback() || port == 0 {
        return Err("local_bind_refused");
    }
    let address = SocketAddr::new(bind, port);
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .map_err(|_| "local_port_unavailable")?;
    socket
        .bind(&address.into())
        .map_err(|_| "local_port_unavailable")?;
    drop(socket);
    Ok(())
}
fn create_preflight(root: &Path, name: &str, bind: IpAddr, port: u16) -> Result<()> {
    local_files::guard(&root.join("uninstall-pending"))?;
    if root.join("uninstall-pending").exists() {
        return Err("uninstall_pending_create_refused");
    }
    if root
        .parent()
        .is_some_and(|parent| parent.join("private/uninstall-pending").exists())
    {
        return Err("uninstall_pending_create_refused");
    }
    let path = record_path(root, name)?;
    local_files::guard(&path)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err("local_name_exists_or_unavailable"),
    }
    check_port(bind, port)
}
pub async fn create(options: Create, base: &Path) -> Result<()> {
    let root = root(Some(&options.root), base)?;
    // The root is explicitly server-only and must already exist, per InstallRoots.
    local_files::guard(&root)?;
    create_preflight(&root, &options.name, options.bind, options.port)?;
    if !options.approve {
        return Err("local_install_approval_required_use_approve_install");
    }
    let startup = crate::agent_startup::choose(options.agent_startup)?;
    crate::agent_startup::preflight(startup)?;
    // Operator-selected Java commonly uses /usr/bin/java -> alternatives -> JDK.
    // Bind the resolved executable, then apply the usual no-link path checks.
    let selected_java = if options.java.is_absolute() {
        options.java.clone()
    } else {
        base.join(&options.java)
    };
    let java = fs::canonicalize(selected_java)
        .map_err(|_| "local_install_java_unavailable_check_java_path")?;
    local_files::guard(&java)?;
    let installer = ServerInstaller::new(
        InstallRoots::new(&root, &[]).map_err(|_| "local_install_root_refused")?,
        TransportConfig::direct(),
    )
    .map_err(|_| "local_installer_unavailable")?;
    prepare(&root)?;
    let _lock = local_files::lock(&root.join("registry.lock"))?;
    create_preflight(&root, &options.name, options.bind, options.port)?;
    let reporter = SilentProgressReporter;
    let selection = Selection {
        version: Some(options.version.clone()),
        loader_version: options.loader_version,
        ..Selection::default()
    };
    let plan = installer
        .plan(options.software, &selection, &java, &reporter)
        .await
        .map_err(|error| match error {
            vesta_server_install::InstallError::RuntimeIncompatible => {
                "local_install_java_incompatible_select_supported_java"
            }
            vesta_server_install::InstallError::RuntimeUnknown
            | vesta_server_install::InstallError::RuntimeFailed => {
                "local_install_java_unavailable_check_java_path"
            }
            _ => "local_install_plan_failed",
        })?;
    println!(
        "install_plan={}; software={}; version={}; warning_count={}; eula=not_accepted",
        plan.hash(),
        plan.software().id(),
        plan.version(),
        plan.warnings().len()
    );
    for warning in plan.warnings() {
        println!("Warning: {}", install_warning(warning));
    }
    let approval = Approval::review(
        &plan,
        Acknowledgments {
            warnings: options.warnings,
            legacy_checksum: options.legacy,
        },
    )
    .map_err(|_| "local_install_acknowledgments_required")?;
    // Point-in-time availability, not a held Minecraft port reservation.
    check_port(options.bind, options.port)?;
    let outcome = installer
        .install(&plan, approval, &reporter)
        .await
        .map_err(|_| "local_install_failed")?;
    let receipt = match outcome {
        vesta_server_install::InstallOutcome::Installed(r) => r,
        vesta_server_install::InstallOutcome::DryRun => return Err("local_install_incomplete"),
    };
    publish(&root, &options.name, &receipt, options.bind, options.port)?;
    println!(
        "created; eula=pending; owner_must_read_and_accept_eula_txt_themselves; no_game_started"
    );
    println!(
        "host_config={}",
        plan.destination().join("host.toml").display()
    );
    drop(_lock);
    if startup == crate::cli::AgentStartupChoice::Boot {
        crate::agent_startup::enable(&plan.destination())?;
        crate::agent_startup::start_registered(&plan.destination())?;
        println!("Agent started; configured for machine boot. Minecraft remains stopped.");
    } else {
        println!(
            "Agent startup: manual. Start it with vesta agent serve <host_config> --allow-local. Minecraft remains stopped."
        );
    }
    Ok(())
}
fn install_warning(warning: &vesta_server_install::Warning) -> &'static str {
    use vesta_server_install::Warning;
    match warning {
        Warning::LegacyChecksum => {
            "Upstream publishes a legacy checksum. Confirm with --ack-legacy-checksum."
        }
        Warning::FirstRunEffects => {
            "Minecraft may extract libraries on its first start. Its EULA stays unaccepted."
        }
        Warning::SourceBuild => {
            "Builds from reviewed sources using Git and a JDK; reference checksum differences are warnings."
        }
        Warning::SourceBuildReferenceMismatch => {
            "The locally built server differs from the reference checksum. Its local hash is recorded."
        }
        Warning::UnsupportedUpstream => "This software is no longer supported upstream.",
        Warning::ExperimentalBuild => "This is an experimental build.",
        Warning::RegionThreading => {
            "This software uses region threading; check plugin compatibility."
        }
        Warning::ProxyResolution => "Downloads use the configured proxy.",
    }
}
fn publish(root: &Path, name: &str, receipt: &Receipt, bind: IpAddr, port: u16) -> Result<()> {
    let directory = root.join("instances").join(&receipt.install_id);
    local_files::guard(&directory)?;
    let game = directory.join("game");
    fs::create_dir(game.join("config")).map_err(|_| "local_destination_exists_or_unavailable")?;
    local_files::write_new(&game.join("eula.txt"), b"# Pending: the owner must read https://www.minecraft.net/eula and accept themselves.\neula=false\n")?;
    local_files::write_new(
        &game.join("server.properties"),
        format!("server-ip={bind}\nserver-port={port}\nonline-mode=true\n").as_bytes(),
    )?;
    let launch = &receipt.launch;
    let artifact = if let Some(path) = &launch.artifact {
        format!(
            "jar = {}\nargs = ['nogui']\n",
            local_files::quoted_path(path)?
        )
    } else {
        format!(
            "argfiles = [{}]\n",
            local_files::quoted_path(launch.argfile.as_ref().ok_or("local_recipe_invalid")?)?
        )
    };
    local_files::write_new(&directory.join("launch.toml"), format!("[minecraft]\nworking-directory = {}\n[launch]\nkind = '{}'\njava-command = {}\nstop = 'console-stop'\n{artifact}", local_files::quoted_path(&game)?, launch.kind, local_files::quoted_path(&launch.java)?).as_bytes())?;
    let config = format!(
        "schema-version = 1\n[paths]\nstate = {}\ngame = {}\nscan-roots = [{}]\nlaunch-definition = {}\n[pack]\nname = {}\nversion = '1.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = {}\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n",
        local_files::quoted_path(&directory.join("state"))?,
        local_files::quoted_path(&game)?,
        local_files::quoted_path(&game.join("config"))?,
        local_files::quoted_path(&directory.join("launch.toml"))?,
        serde_json::to_string(name).map_err(|_| "local_name_invalid")?,
        serde_json::to_string(&receipt.version).map_err(|_| "local_recipe_invalid")?
    );
    local_files::write_new(&directory.join("host.toml"), config.as_bytes())?;
    local_files::write_new(&directory.join("local-name"), name.as_bytes())?;
    let record = serde_json::to_vec(&Record {
        schema: 1,
        instance: receipt.install_id.clone(),
    })
    .map_err(|_| "local_record_invalid")?;
    // Publish the name last. Failure retains the unregistered install for manual recovery.
    local_files::write_new(&record_path(root, name)?, &record)
}
/// Stable lease outside the movable instance. Legacy non-managed configs have
/// only the existing runtime runner lock and are not eligible for named deletion.
pub fn control_name(source: &Path) -> Result<String> {
    let directory = source.parent().ok_or("local_path_refused")?;
    if managed_lease(directory).is_some() {
        let bytes = local_files::read(&directory.join("local-name"), 48)?;
        let name = std::str::from_utf8(&bytes).map_err(|_| "local_name_invalid")?;
        if !crate::cli::valid_name(name) {
            return Err("local_name_invalid");
        }
        Ok(name.to_owned())
    } else {
        // Legacy serve configs still work; display-name metadata is not a
        // management target, identity proof or administrative grant.
        Ok("local".into())
    }
}
pub fn managed_lease(directory: &Path) -> Option<PathBuf> {
    let id = directory.file_name()?.to_str()?;
    if directory.parent()?.file_name()? != "instances"
        || id.len() != 36
        || !id.starts_with("srv_")
        || !id[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    Some(directory.with_extension("operator-lock"))
}
pub fn delete(root: &Path, name: &str, confirm: &str) -> Result<PathBuf> {
    let _registry = local_files::lock(&root.join("registry.lock"))?;
    delete_guarded(root, name, confirm)
}
/// Caller holds the registry lock, including any reviewed uninstall snapshot.
pub(crate) fn delete_guarded(root: &Path, name: &str, confirm: &str) -> Result<PathBuf> {
    if name != confirm {
        return Err("local_delete_confirmation_mismatch");
    }
    // No preparation on refusals: the name must already resolve.
    let server = resolve(root, name)?;
    let _lease =
        local_files::lock(&managed_lease(&server.directory).ok_or("local_record_invalid")?)?;
    // Released OS locks do not prove an orphaned game stopped after a crash.
    crate::sessions::require_stopped(&server.directory)?;
    // Also excludes an existing runner started without this CLI's folder lease.
    let state = StateDirectory::new(
        server.directory.join("state"),
        Some(&server.directory.join("game")),
    )
    .map_err(|_| "local_state_refused")?;
    // A supervised game outlives its agent: a live supervisor or recorded game refuses.
    // Check the configured state as well as the default; an edited host.toml may point
    // elsewhere. An unreadable configuration with supervision unknown refuses.
    let mut states = vec![state.clone()];
    if server.config.exists() {
        let configured =
            vesta_control::config::HostConfiguration::load_file(&server.config, &server.directory)
                .map_err(|_| "local_config_unreadable_delete_refused")?;
        states.push(
            StateDirectory::new(configured.state(), Some(configured.game()))
                .map_err(|_| "local_state_refused")?,
        );
    }
    for state in &states {
        if state.path().is_dir()
            && vesta_supervisor::instance_busy(state).map_err(|_| "local_state_refused")?
        {
            return Err("local_supervised_game_running");
        }
    }
    let db = if state.database_file().exists() {
        let runner = state
            .acquire_runner_lock()
            .map_err(|_| "local_server_busy_or_running")?;
        let db = StateDatabase::open(state.clone(), Arc::new(SystemClock))
            .map_err(|_| "local_state_refused")?;
        Some((db, runner))
    } else {
        None
    };
    let recovery_root = root.join("recoverable");
    crate::agent_startup::disable_quiescent(&server.directory)?;
    local_files::guard(&recovery_root)?;
    if !recovery_root.is_dir() {
        return Err("local_recovery_unavailable");
    }
    let recovery = recovery_root.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&recovery).map_err(|_| "local_recovery_unavailable")?;
    if let Some((db, _)) = &db {
        let audit = AuditLog::new(db.clone()).map_err(|_| "local_audit_failed")?;
        let record = AuditRecord::of(
            AuditActor::system("os-local-cli".into()).map_err(|_| "local_audit_failed")?,
            "local.server.delete".into(),
            "named-server".into(),
            "move_to_recovery_admitted".into(),
            None,
        )
        .map_err(|_| "local_audit_failed")?;
        audit
            .append_record(&record)
            .map_err(|_| "local_audit_failed")?;
    }
    let had_state = db.is_some();
    // Close SQLite and native lock handles before rename (Windows sharing rules).
    if let Some((db, runner)) = db {
        db.close().map_err(|_| "local_state_refused")?;
        drop(runner);
    }
    // The external instance lease remains held across rename; every agent
    // serving this managed source/game takes it before opening the runtime.
    // Keep the registry lease across both moves. No recursive removal anywhere.
    fs::rename(&server.directory, recovery.join("server"))
        .map_err(|_| "local_recovery_move_failed")?;
    fs::rename(record_path(root, name)?, recovery.join("name.json"))
        .map_err(|_| "local_recovery_record_move_failed")?;
    if had_state {
        let moved = recovery.join("server");
        let state = StateDirectory::new(moved.join("state"), Some(&moved.join("game")))
            .map_err(|_| "local_recovery_completed_audit_outcome_failed")?;
        let db = StateDatabase::open(state, Arc::new(SystemClock))
            .map_err(|_| "local_recovery_completed_audit_outcome_failed")?;
        let outcome = (|| {
            let audit = AuditLog::new(db.clone())?;
            let record = AuditRecord::of(
                AuditActor::system("os-local-cli".into())?,
                "local.server.delete".into(),
                "named-server".into(),
                "moved_to_recovery".into(),
                None,
            )?;
            audit.append_record(&record).map(|_| ())
        })();
        let closed = db.close();
        if outcome.is_err() || closed.is_err() {
            return Err("local_recovery_completed_audit_outcome_failed");
        }
    }
    Ok(recovery)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        prepare(&root).unwrap();
        let id = format!("srv_{}", uuid::Uuid::new_v4().simple());
        let instance = root.join("instances").join(&id);
        fs::create_dir_all(instance.join("game/world")).unwrap();
        fs::write(instance.join("game/world/keep"), b"owned fixture").unwrap();
        fs::write(
            root.join("names/owned.json"),
            serde_json::to_vec(&Record {
                schema: 1,
                instance: id,
            })
            .unwrap(),
        )
        .unwrap();
        (temp, root)
    }
    #[test]
    fn delete_refuses_unproven_shutdown_without_moving_or_erasing_files() {
        let (_temp, root) = fixture();
        let server = resolve(&root, "owned").unwrap();
        let session = crate::sessions::Sessions::begin([server.directory.clone()], false).unwrap();
        drop(session); // Simulates retained evidence, not a real agent/game crash.
        assert_eq!(
            delete(&root, "owned", "owned"),
            Err("local_shutdown_unproven_requires_supervisor_recovery")
        );
        assert_eq!(resolve(&root, "owned").unwrap().directory, server.directory);
        assert_eq!(
            fs::read(server.directory.join("game/world/keep")).unwrap(),
            b"owned fixture"
        );
        assert!(server.directory.join("operator-session-pending").is_file());
        assert_eq!(fs::read_dir(root.join("recoverable")).unwrap().count(), 0);
    }
    #[test]
    fn fixture_publication_writes_pending_eula_and_valid_host_and_launch_documents() {
        let (_temp, root) = fixture();
        let server = resolve(&root, "owned").unwrap();
        let id = server
            .directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let artifact = server.directory.join("software/vanilla.jar");
        fs::create_dir(server.directory.join("software")).unwrap();
        fs::write(&artifact, b"inert not executable").unwrap();
        let java = root.join("inert-java-placeholder");
        fs::write(&java, b"inert never launched").unwrap();
        let receipt = Receipt {
            built: None,
            schema: 1,
            processed: None,
            install_id: id,
            plan_hash: "a".repeat(64),
            software: vesta_server_install::Software::Vanilla,
            version: "1.21.1".into(),
            build: None,
            published_digest: serde_json::from_str(&format!(
                r#"{{"algorithm":"sha256","hex":"{}"}}"#,
                "b".repeat(64)
            ))
            .unwrap(),
            published_size: Some(20),
            observed_sha256: "b".repeat(64),
            actual_size: 20,
            java_requirement: vesta_server_install::JavaRequirement {
                major: 21,
                evidence: "inert fixture".into(),
            },
            launch: vesta_server_install::LaunchRecipe {
                kind: "java-jar".into(),
                java,
                working_directory: server.directory.join("game"),
                artifact: Some(artifact),
                args: vec!["nogui".into()],
                stop_mode: "console-stop".into(),
                argfile: None,
            },
            eula: "not_accepted".into(),
            warnings: vec![],
            completed_at: 0,
            bundle: None,
        };
        // Another name for the same owned fixture exercises publication last;
        // the test never invokes plan/install or probes the inert Java placeholder.
        publish(
            &root,
            "new-name",
            &receipt,
            "127.0.0.1".parse().unwrap(),
            25565,
        )
        .unwrap();
        assert!(
            String::from_utf8(fs::read(server.directory.join("game/eula.txt")).unwrap())
                .unwrap()
                .contains("eula=false")
        );
        assert!(!local_files::eula_accepted(&server.directory.join("game")));
        let config =
            vesta_control::config::HostConfiguration::load_file(&server.config, &root).unwrap();
        config.validate_host_paths().unwrap();
        assert!(matches!(
            config.listener(),
            vesta_control::config::ListenerDescription::Disabled
        ));
        vesta_game_runtime::launch_config::LaunchConfiguration::parse_toml(
            &fs::read(server.directory.join("launch.toml")).unwrap(),
            &root,
        )
        .unwrap();
        assert_eq!(
            fs::read(server.directory.join("game/world/keep")).unwrap(),
            b"owned fixture"
        );
        assert_eq!(
            resolve(&root, "new-name").unwrap().directory,
            server.directory
        );
    }
    #[test]
    fn delete_refuses_existing_runner_and_preserves_audit_in_recovery() {
        let (_temp, root) = fixture();
        let server = resolve(&root, "owned").unwrap();
        let state = StateDirectory::new(
            server.directory.join("state"),
            Some(&server.directory.join("game")),
        )
        .unwrap();
        let db = StateDatabase::open(state.clone(), Arc::new(SystemClock)).unwrap();
        let runner = state.acquire_runner_lock().unwrap();
        assert_eq!(
            delete(&root, "owned", "owned"),
            Err("local_server_busy_or_running")
        );
        drop(runner);
        db.close().unwrap();
        let recovery = delete(&root, "owned", "owned").unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(
                recovery.join("server/state"),
                Some(&recovery.join("server/game")),
            )
            .unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let audit = AuditLog::new(db.clone()).unwrap();
        assert!(audit.verify_chain().unwrap().valid);
        db.read(|c| {
            let count: i64 = c.query_row(
                "SELECT count(*) FROM audit_events WHERE action='local.server.delete'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(count, 2);
            Ok(())
        })
        .unwrap();
        drop(audit);
        db.close().unwrap();
        assert_eq!(
            fs::read(recovery.join("server/game/world/keep")).unwrap(),
            b"owned fixture"
        );
    }
    #[test]
    fn bound_port_is_refused_and_public_address_never_bound() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(
            check_port(addr.ip(), addr.port()),
            Err("local_port_unavailable")
        );
        drop(listener);
        assert!(check_port(addr.ip(), addr.port()).is_ok());
        assert_eq!(
            check_port("0.0.0.0".parse().unwrap(), 25565),
            Err("local_bind_refused")
        );
    }
    #[test]
    fn create_existing_name_and_delete_confirmation_refused() {
        let (_temp, root) = fixture();
        assert_eq!(
            create_preflight(&root, "owned", "127.0.0.1".parse().unwrap(), 25565),
            Err("local_name_exists_or_unavailable")
        );
        assert_eq!(
            delete(&root, "owned", "different"),
            Err("local_delete_confirmation_mismatch")
        );
        assert!(resolve(&root, "owned").is_ok());
    }
    #[test]
    fn delete_running_lease_refused_then_moves_everything_without_erasure() {
        let (_temp, root) = fixture();
        let server = resolve(&root, "owned").unwrap();
        let lease = local_files::lock(&managed_lease(&server.directory).unwrap()).unwrap();
        assert_eq!(
            delete(&root, "owned", "owned"),
            Err("local_server_busy_or_running")
        );
        drop(lease);
        let recovery = delete(&root, "owned", "owned").unwrap();
        assert_eq!(
            fs::read(recovery.join("server/game/world/keep")).unwrap(),
            b"owned fixture"
        );
        assert!(recovery.join("name.json").is_file());
        assert!(!server.directory.exists());
    }
}
