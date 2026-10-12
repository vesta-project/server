//! Offline, protected OS-local recovery commands. Never a remote authority seam.
use crate::{
    local_files::{self, Result},
    servers,
};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use vesta_control::config::HostConfiguration;
use vesta_state::{StateDatabase, StateDirectory, SystemClock, export::ExportSummary};

struct OfflineServer {
    config: HostConfiguration,
    // Keep leases alive through opening, exporting/resolving and closing the DB.
    _leases: Vec<File>,
}

impl OfflineServer {
    fn open(root: &Path, name: &str) -> Result<Self> {
        let server = servers::resolve(root, name)?;
        let config = HostConfiguration::load_file(&server.config, &server.directory)
            .map_err(|e| e.code())?;
        let source = local_files::absolute(&server.config, &server.directory)?;
        // Exactly serve's source/game-parent lease set, acquired before the DB.
        let paths = [source.parent(), config.game().parent()]
            .into_iter()
            .flatten()
            .filter_map(servers::managed_lease)
            .collect::<BTreeSet<_>>();
        let leases = paths
            .iter()
            .map(|p| local_files::lock(p))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            config,
            _leases: leases,
        })
    }

    fn database(&self) -> Result<StateDatabase> {
        let state = StateDirectory::new(self.config.state(), Some(self.config.game()))
            .map_err(|e| e.code())?;
        StateDatabase::open(state, Arc::new(SystemClock)).map_err(|e| e.code())
    }
}

pub fn recovery_export(
    root: &Path,
    name: &str,
    output: &Path,
    base: &Path,
) -> Result<ExportSummary> {
    let server = OfflineServer::open(root, name)?;
    let output = local_files::absolute(output, base)?;
    if contained(&output, server.config.game())? || contained(&output, server.config.state())? {
        return Err("local_export_destination_refused");
    }
    let db = server.database()?;
    let result = publish_export(&db, &output);
    let closed = db.close().map_err(|e| e.code());
    let summary = result?;
    closed?;
    Ok(summary)
}

pub fn backup_resolve(
    root: &Path,
    name: &str,
    job: Option<&str>,
    confirm: Option<&str>,
) -> Result<()> {
    let server = OfflineServer::open(root, name)?;
    if job != confirm {
        return Err("local_resolve_confirmation_mismatch");
    }
    let db = server.database()?;
    let result = if let Some(id) = job {
        vesta_control::backups::resolve_recovery_locally(&db, id)
            .map_err(|e| e.code())
            .map(|()| println!("backup_recovery_resolved"))
    } else {
        vesta_control::backups::recovery_jobs(&db)
            .map_err(|e| e.code())
            .map(|jobs| {
                if jobs.is_empty() {
                    println!("no_restore_needs_recovery");
                }
                for (id, error) in jobs {
                    println!("{id} {}", error.as_deref().unwrap_or("none"));
                }
            })
    };
    let closed = db.close().map_err(|e| e.code());
    result?;
    closed
}

fn contained(path: &Path, directory: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        // Account for the ordinary/verbatim disk spelling and ASCII case. This is
        // a lexical check plus local_files::guard, not a same-account race boundary.
        let normalize = |p: &Path| -> Result<String> {
            Ok(p.to_str()
                .ok_or("local_path_refused")?
                .trim_start_matches(r"\\?\")
                .replace('\\', "/")
                .to_lowercase()
                .trim_end_matches('/')
                .to_owned())
        };
        let path = normalize(path)?;
        let directory = normalize(directory)?;
        Ok(path == directory || path.starts_with(&format!("{directory}/")))
    }
    #[cfg(not(windows))]
    {
        Ok(path.starts_with(directory))
    }
}

struct Unpublished {
    output: PathBuf,
    temporary: Option<PathBuf>,
    published: bool,
}
impl Drop for Unpublished {
    fn drop(&mut self) {
        if let Some(path) = &self.temporary {
            let _ = fs::remove_file(path);
        }
        if !self.published {
            let _ = fs::remove_file(&self.output);
        }
    }
}

fn publish_export(db: &StateDatabase, output: &Path) -> Result<ExportSummary> {
    local_files::guard(output)?;
    let parent = output.parent().ok_or("local_export_destination_refused")?;
    // Reserve the final name with create_new: pre-existing files are never replaced.
    // The empty reservation is replaced only by our fully flushed sibling file.
    let reservation = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|_| "local_destination_exists_or_unavailable")?;
    let mut cleanup = Unpublished {
        output: output.to_owned(),
        temporary: None,
        published: false,
    };
    drop(reservation);
    let temporary = parent.join(format!(".vesta-recovery-{}.tmp", uuid::Uuid::new_v4()));
    local_files::guard(&temporary)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| "local_write_failed")?;
    cleanup.temporary = Some(temporary.clone());
    let mut writer = BufWriter::new(file);
    let summary =
        vesta_state::export::write_recovery_export(db, &mut writer).map_err(|e| e.code())?;
    writer.flush().map_err(|_| "local_write_failed")?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|_| "local_write_failed")?;
    drop(writer);
    fs::rename(&temporary, output).map_err(|_| "local_write_failed")?;
    cleanup.temporary = None;
    cleanup.published = true;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_publishes_complete_json_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let output = temp.path().join("export.json");
        let summary = publish_export(&db, &output).unwrap();
        let bytes = fs::read(&output).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["format"], "vesta-recovery-export");
        assert_eq!(summary.tables, 20);
        assert_eq!(
            publish_export(&db, &output),
            Err("local_destination_exists_or_unavailable")
        );
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
        db.close().unwrap();
        assert_eq!(
            publish_export(&db, &temp.path().join("failed.json")),
            Err("state_closed")
        );
        assert!(!temp.path().join("failed.json").exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let id = format!("srv_{}", uuid::Uuid::new_v4().simple());
        let instance = root.join("instances").join(&id);
        fs::create_dir_all(instance.join("game")).unwrap();
        fs::create_dir(root.join("names")).unwrap();
        fs::write(
            root.join("names/owned.json"),
            serde_json::json!({"schema":1,"instance":id}).to_string(),
        )
        .unwrap();
        fs::write(instance.join("host.toml"), "schema-version = 1\n[paths]\nstate = 'state'\ngame = 'game'\nscan-roots = ['game/config']\nlaunch-definition = 'launch.toml'\n[pack]\nname = 'Fixture'\nversion = '1.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n").unwrap();
        (temp, root, instance)
    }

    #[test]
    fn offline_commands_refuse_agent_lease_before_opening_state() {
        let (_temp, root, instance) = fixture();
        let lease = local_files::lock(&servers::managed_lease(&instance).unwrap()).unwrap();
        assert_eq!(
            recovery_export(&root, "owned", &root.join("out.json"), &root),
            Err("local_server_busy_or_running")
        );
        assert_eq!(
            backup_resolve(&root, "owned", None, None),
            Err("local_server_busy_or_running")
        );
        assert!(!instance.join("state").exists());
        drop(lease);
        assert!(backup_resolve(&root, "owned", None, None).is_ok());
        assert_eq!(
            backup_resolve(&root, "owned", Some("job"), Some("other")),
            Err("local_resolve_confirmation_mismatch")
        );
        assert_eq!(
            backup_resolve(&root, "owned", Some("missing"), Some("missing")),
            Err("backup_job_unknown")
        );
        assert_eq!(
            recovery_export(&root, "owned", &instance.join("game/out.json"), &root),
            Err("local_export_destination_refused")
        );
        assert_eq!(
            recovery_export(&root, "owned", &instance.join("state/out.json"), &root),
            Err("local_export_destination_refused")
        );
        assert!(recovery_export(&root, "owned", &root.join("out.json"), &root).is_ok());
    }

    #[test]
    fn lexical_containment_matches_whole_components() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        assert!(contained(&state, &state).unwrap());
        assert!(contained(&state.join("out.json"), &state).unwrap());
        assert!(!contained(&temp.path().join("state-other/out.json"), &state).unwrap());
        #[cfg(windows)]
        assert!(
            contained(
                &PathBuf::from(state.to_str().unwrap().to_uppercase()).join("out.json"),
                &state
            )
            .unwrap()
        );
    }
}
