//! Trusted local composition for stopped software activation.
use crate::{
    config::HostConfiguration,
    files,
    operations::{LocalRuntime, RuntimeHandle, StartBlock},
    software::{self, RuntimeHooks},
};
use std::{
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};
use vesta_game_runtime::launch_config::LaunchConfiguration;
use vesta_server_install::{InstallRoots, ManagedLaunch, ServerInstaller, TransportConfig};
use vesta_state::StateDatabase;
type Result<T> = software::Result<T>;
fn unavailable<T>(_: T) -> software::SoftwareError {
    software::SoftwareError::RuntimeUnavailable
}

struct Hooks {
    maintenance: Arc<tokio::sync::Mutex<()>>,
    config: HostConfiguration,
    db: StateDatabase,
    runtime: RuntimeHandle,
    local: LocalRuntime,
    jvm_args: Vec<String>,
    recovery: Mutex<Option<StartBlock>>,
}
struct Held {
    _maintenance: tokio::sync::OwnedMutexGuard<()>,
    hooks: Arc<Hooks>,
    hold: Arc<files::StoppedGuard>,
}
impl software::StoppedGuard for Held {
    fn activate(&mut self, document: &str) -> Result<()> {
        self.hold.check().map_err(unavailable)?;
        let parsed = LaunchConfiguration::parse_toml(document.as_bytes(), self.hooks.config.base())
            .map_err(unavailable)?;
        if !files::same_directory(parsed.working_directory(), self.hooks.config.game())
            .map_err(unavailable)?
        {
            return Err(software::SoftwareError::InvalidRequest);
        }
        let identity = identity(
            &self.hooks.config,
            document.as_bytes(),
            parsed.working_directory(),
        );
        let launch = parsed.into_launch_spec(None).map_err(unavailable)?;
        let staged = vesta_supervisor::reconfigure::stage_launch(
            self.hooks.db.directory(),
            document.as_bytes(),
        )
        .map_err(unavailable)?;
        atomic_document(self.hooks.config.launch_definition(), document.as_bytes())
            .map_err(unavailable)?;
        self.hold.check().map_err(unavailable)?;
        self.hooks
            .local
            .replace_stopped_launch(launch, identity, staged, self.hold.supervisor_token())
            .map_err(unavailable)?;
        self.hold.check().map_err(unavailable)
    }
}
impl RuntimeHooks for Arc<Hooks> {
    fn launch_document(&self, launch: &ManagedLaunch) -> Result<String> {
        if !files::same_directory(&launch.working_directory, self.config.game())
            .map_err(unavailable)?
        {
            return Err(software::SoftwareError::InvalidRequest);
        }
        let paths = launch
            .required_files
            .iter()
            .map(|p| p.to_str().ok_or(software::SoftwareError::InvalidRequest))
            .collect::<Result<Vec<_>>>()?;
        let mut launch_table = toml::map::Map::new();
        launch_table.insert("kind".into(), toml::Value::String("native".into()));
        launch_table.insert(
            "executable".into(),
            toml::Value::String(
                launch
                    .java
                    .to_str()
                    .ok_or(software::SoftwareError::InvalidRequest)?
                    .replace('\\', "/"),
            ),
        );
        launch_table.insert(
            "args".into(),
            toml::Value::Array(
                launch
                    .args
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
        launch_table.insert(
            "jvm-args".into(),
            toml::Value::Array(
                self.jvm_args
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
        launch_table.insert(
            "required-files".into(),
            toml::Value::Array(
                paths
                    .into_iter()
                    .map(|p| toml::Value::String(p.replace('\\', "/")))
                    .collect(),
            ),
        );
        launch_table.insert("stop".into(), toml::Value::String("console-stop".into()));
        let mut minecraft = toml::map::Map::new();
        minecraft.insert(
            "working-directory".into(),
            toml::Value::String(
                self.config
                    .game()
                    .to_str()
                    .ok_or(software::SoftwareError::InvalidRequest)?
                    .replace('\\', "/"),
            ),
        );
        let mut document = toml::map::Map::new();
        document.insert("minecraft".into(), toml::Value::Table(minecraft));
        document.insert("launch".into(), toml::Value::Table(launch_table));
        let text = toml::to_string(&toml::Value::Table(document)).map_err(unavailable)?;
        LaunchConfiguration::parse_toml(text.as_bytes(), self.config.base())
            .map_err(unavailable)?;
        Ok(text)
    }
    fn acquire_stopped(&self) -> Result<Box<dyn software::StoppedGuard>> {
        let maintenance = self.maintenance.clone().try_lock_owned().map_err(|_|software::SoftwareError::Busy)?;
        let hold = files::StoppedGuard::acquire(&self.runtime, self.db.directory())
            .map_err(unavailable)?;
        if self
            .runtime
            .supervision()
            .map_err(unavailable)?
            .is_some_and(|health| health.restart_pending_ms.is_some())
        {
            return Err(software::SoftwareError::Busy);
        }
        Ok(Box::new(Held {
            _maintenance: maintenance,
            hooks: self.clone(),
            hold,
        }))
    }
    fn recovery_block(&self, blocked: bool) -> Result<()> {
        let mut recovery = self.recovery.lock().map_err(unavailable)?;
        if blocked && recovery.is_none() {
            *recovery = Some(self.runtime.block_starts());
        } else if !blocked {
            recovery.take();
        }
        Ok(())
    }
}
pub(crate) fn open(
    config: &HostConfiguration,
    db: &StateDatabase,
    runtime: RuntimeHandle,
    local: LocalRuntime,
    maintenance: Arc<tokio::sync::Mutex<()>>,
) -> Result<Option<software::SoftwareService>> {
    let bytes =
        files::read_local_document(config.launch_definition(), 65_536).map_err(unavailable)?;
    let original = String::from_utf8(bytes).map_err(unavailable)?;
    let parsed = LaunchConfiguration::parse_toml(original.as_bytes(), config.base())
        .map_err(unavailable)?
        .into_launch_spec(None)
        .map_err(unavailable)?;
    let java = parsed.literal_program().to_owned();
    let java_name = java
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !java_name.eq_ignore_ascii_case("java") && !java_name.eq_ignore_ascii_case("java.exe") {
        // An allowlisted, probed custom Java can have a different filename.
        // Only an exact protected current software-state document permits
        // that selected executable across agent restarts and software switches.
        let selected = known_managed_launch(db, &original);
        if !selected {
            return Ok(None);
        }
    }
    let tree: toml::Value = toml::from_str(&original).map_err(unavailable)?;
    let jvm_args = tree
        .get("launch")
        .and_then(|t| t.get("jvm-args"))
        .and_then(toml::Value::as_array)
        .map(|args| {
            args.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or(software::SoftwareError::InvalidRequest)
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    // Absolute fixture/instance paths must not put downloads in the caller's cwd.
    // In particular, management smoke tools may run from the repository root.
    let root = config
        .state()
        .parent()
        .ok_or(software::SoftwareError::InvalidRequest)?
        .join("software-installations");
    vesta_state::StateDirectory::new(&root, None)
        .and_then(|directory| directory.prepare())
        .map_err(unavailable)?;
    let installer = ServerInstaller::new(
        InstallRoots::new(
            &root,
            &[config.game().to_owned(), config.state().to_owned()],
        )?,
        TransportConfig::direct(),
    )?;
    // Cache failure is an optimization failure, not a reason to disable software
    // management. The installer retains its bounded in-memory cache as fallback.
    let installer = match software::catalogue_store::open(db.clone()) {
        Ok(store) => installer.with_catalogue_store(store),
        Err(_) => installer,
    };
    let hooks = Arc::new(Hooks {
        maintenance,
        config: config.clone(),
        db: db.clone(),
        runtime,
        local,
        jvm_args,
        recovery: Mutex::new(None),
    });
    software::SoftwareService::open(
        db.clone(),
        Arc::new(installer),
        java,
        config.game().to_owned(),
        original,
        Arc::new(hooks),
    )
    .map(Some)
}
pub(crate) fn identity(config: &HostConfiguration, bytes: &[u8], cwd: &Path) -> String {
    vesta_supervisor::launch_identity(&[
        bytes,
        config.base().to_string_lossy().as_bytes(),
        config.launch_definition().to_string_lossy().as_bytes(),
        config.game().to_string_lossy().as_bytes(),
        config.state().to_string_lossy().as_bytes(),
        cwd.to_string_lossy().as_bytes(),
    ])
}
fn atomic_document(path: &Path, bytes: &[u8]) -> files::Result<()> {
    files::read_local_document(path, 65_536)?;
    let parent = path.parent().ok_or(files::FileError::InvalidPath)?;
    let stage = parent.join(format!(
        ".vesta-launch-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&stage)
        .map_err(|_| files::FileError::Unavailable)?;
    vesta_state::StateDirectory::new(parent, None)
        .and_then(|directory| directory.tighten_file(&stage))
        .map_err(|_| files::FileError::Unavailable)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| files::FileError::Unavailable)?;
    drop(file);
    files::read_local_document(path, 65_536)?;
    fs::rename(&stage, path).map_err(|_| files::FileError::Unavailable)?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| files::FileError::Unavailable)?;
    Ok(())
}

/// Admit observation/recovery only when both definitions are already journaled.
/// This never adopts, rewrites or launches either candidate.
pub(crate) fn recovery_binding(
    config: &HostConfiguration,
    observed: &str,
) -> std::result::Result<bool, crate::host::HostError> {
    use crate::host::HostError;
    use rusqlite::OptionalExtension;
    let state = vesta_state::StateDirectory::new(config.state(), Some(config.game()))
        .map_err(HostError::State)?;
    if !state.path().is_dir() {
        return Ok(false);
    }
    let db = StateDatabase::open(state.clone(), Arc::new(vesta_state::SystemClock))
        .map_err(HostError::State)?;
    let result=db.read_with(|tx|->Result<bool>{
        let c=tx.connection();
        let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='software_state')",[],|r|r.get(0))?;
        if !exists{return Ok(false);}
        let state:Option<(bool,String,Option<String>)>=c.query_row("SELECT recovery_required,current_launch,previous_launch FROM software_state WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((required,current,previous))=state else{return Ok(false);};
        let pending:Option<String>=c.query_row("SELECT candidate_launch FROM software_jobs WHERE state IN ('activating','recovery_required') AND candidate_launch IS NOT NULL ORDER BY updated_at_ms DESC LIMIT 1",[],|r|r.get(0)).optional()?;
        if !required&&pending.is_none(){return Ok(false);}
        let actual=files::read_local_document(config.launch_definition(),65_536).map_err(unavailable)?;
        let mut actual_known=false;let mut supervisor_known=false;
        for value in [Some(current),previous,pending].into_iter().flatten() {
            if value.len()>1024*1024{return Err(software::SoftwareError::StateUnavailable);}
            let stored:serde_json::Value=serde_json::from_str(&value).map_err(unavailable)?;
            let Some(document)=stored.get("document").and_then(serde_json::Value::as_str) else{return Err(software::SoftwareError::StateUnavailable);};
            let parsed=LaunchConfiguration::parse_toml(document.as_bytes(),config.base()).map_err(unavailable)?;
            if !files::same_directory(parsed.working_directory(),config.game()).map_err(unavailable)?{return Ok(false);}
            actual_known|=actual==document.as_bytes();
            supervisor_known|=identity(config,document.as_bytes(),parsed.working_directory())==observed;
        }
        Ok(actual_known&&supervisor_known)
    }).map_err(|_|HostError::Supervisor("software_recovery_binding_unavailable"));
    db.close().map_err(HostError::State)?;
    result
}

fn known_managed_launch(db: &StateDatabase, document: &str) -> bool {
    db.read(|c|c.query_row("SELECT current_launch FROM software_state WHERE singleton=1 AND length(CAST(current_launch AS BLOB))<=1048576",[],|r|r.get::<_,String>(0)).map_err(Into::into))
        .ok().and_then(|raw|serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|value|value.get("document").and_then(serde_json::Value::as_str)==Some(document))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renamed_java_requires_the_exact_protected_current_definition() {
        let temp = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap(),
            Arc::new(vesta_state::SystemClock),
        )
        .unwrap();
        assert!(!known_managed_launch(&db, "selected"));
        db.write(|c| {
            c.execute_batch("CREATE TABLE software_state(singleton INTEGER,current_launch TEXT)")?;
            c.execute(
                "INSERT INTO software_state VALUES(1,?)",
                [serde_json::json!({"document":"selected"}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(known_managed_launch(&db, "selected"));
        assert!(!known_managed_launch(&db, "external edit"));
        db.write(|c| {
            c.execute(
                "UPDATE software_state SET current_launch='invalid json'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(!known_managed_launch(&db, "selected"));
    }
}
