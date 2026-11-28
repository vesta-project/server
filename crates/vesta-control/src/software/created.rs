//! Trusted create/import facts and conservative startup adoption.
use super::{Installation, Result, SoftwareError, model::StoredLaunch, store};
use crate::config::HostConfiguration;
use std::{path::Path, sync::Arc};
use vesta_server_install::ManagedArtifact;
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

pub(crate) fn seed(
    dir: &Path,
    document: String,
    installation: Installation,
    artifacts: Vec<ManagedArtifact>,
) -> Result<()> {
    let directory = StateDirectory::new(dir.join("private/state"), Some(&dir.join("game")))
        .map_err(|_| SoftwareError::StateUnavailable)?;
    let db = StateDatabase::open(directory, Arc::new(SystemClock))
        .map_err(|_| SoftwareError::StateUnavailable)?;
    let result = record(
        &db,
        StoredLaunch {
            installation,
            document,
            artifacts,
            warnings: Vec::new(),
        },
    );
    db.close().map_err(|_| SoftwareError::StateUnavailable)?;
    result
}

fn record(db: &StateDatabase, launch: StoredLaunch) -> Result<()> {
    store::initialize(db, &launch)?;
    db.write_with(|tx| -> Result<()> {
        let c = tx.connection();
        let state = store::state(c)?;
        let jobs: i64 = c.query_row("SELECT count(*) FROM software_jobs", [], |r| r.get(0))?;
        if state.revision == 0
            && !state.recovery
            && state.previous.is_none()
            && jobs == 0
            && state.current.installation.software.is_none()
            && state.current.document == launch.document
        {
            c.execute(
                "UPDATE software_state SET current_launch=? WHERE singleton=1",
                [store::encode(&launch)?],
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Only host-created instance layouts are eligible. Arbitrary jars and edited
/// entry points are deliberately left unknown. The current document is retained.
pub(crate) fn backfill(
    config: &HostConfiguration,
    db: &StateDatabase,
    document: &str,
) -> Result<()> {
    use vesta_server_install::Software;
    let pack = config.pack();
    if pack.minecraft_version().is_empty() {
        return Ok(());
    }
    let host: toml::Value = match std::fs::read_to_string(config.base().join("host.toml"))
        .ok()
        .and_then(|v| toml::from_str(&v).ok())
    {
        Some(v) => v,
        None => return Ok(()),
    };
    let loader = host.get("pack").and_then(|v| v.get("loader"));
    let (software, loader_version) = match loader {
        None => (Software::Vanilla, None),
        Some(l) => {
            let Some(kind) = l.get("kind").and_then(|v| v.as_str()) else {
                return Ok(());
            };
            let Some(version) = l
                .get("version")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
            else {
                return Ok(());
            };
            let software = match kind {
                "fabric" => Software::Fabric,
                "quilt" => Software::Quilt,
                _ => return Ok(()),
            };
            (software, Some(version.to_owned()))
        }
    };
    let tree: toml::Value = toml::from_str(document).map_err(|_| SoftwareError::InvalidRequest)?;
    let Some(args) = tree
        .get("launch")
        .and_then(|v| v.get("args"))
        .and_then(|v| v.as_array())
    else {
        return Ok(());
    };
    let Some(args) = args.iter().map(|v| v.as_str()).collect::<Option<Vec<_>>>() else {
        return Ok(());
    };
    let instances = config.base().join("install/instances");
    let valid_path = |text: &str, suffix: &str| -> bool {
        let path = Path::new(text);
        let Ok(rest) = path.strip_prefix(&instances) else {
            return false;
        };
        rest.components().count() == 3 && rest.ends_with(suffix) && path.is_file()
    };
    let valid = match software {
        Software::Vanilla => {
            args.len() == 3
                && args[0] == "-jar"
                && valid_path(args[1], "software/vanilla.jar")
                && args[2] == "nogui"
        }
        Software::Fabric | Software::Quilt => {
            let (property, main) = if software == Software::Fabric {
                (
                    "-Dfabric.gameJarPath=",
                    "net.fabricmc.loader.impl.launch.knot.KnotServer",
                )
            } else {
                (
                    "-Dloader.gameJarPath=",
                    "org.quiltmc.loader.impl.launch.knot.KnotServer",
                )
            };
            let jar = args
                .first()
                .and_then(|v| v.strip_prefix(property))
                .map(Path::new);
            let instance = jar.and_then(|p| p.parent()).and_then(|p| p.parent());
            let receipt = instance
                .and_then(|p| std::fs::read(p.join("control/receipt.json")).ok())
                .and_then(|v| {
                    serde_json::from_slice::<vesta_server_install::BundleReceipt>(&v).ok()
                });
            args.len() == 5
                && args[0]
                    .strip_prefix(property)
                    .is_some_and(|p| valid_path(p, "software/vanilla.jar"))
                && args[1] == "-cp"
                && args[3] == main
                && args[4] == "nogui"
                && receipt
                    .as_ref()
                    .zip(instance)
                    .is_some_and(|(receipt, instance)| {
                        receipt.software == software
                            && receipt.version == pack.minecraft_version()
                            && Some(&receipt.loader_version) == loader_version.as_ref()
                            && args[2]
                                == receipt
                                    .artifacts
                                    .iter()
                                    .skip(1)
                                    .map(|a| instance.join(&a.path).to_string_lossy().into_owned())
                                    .collect::<Vec<_>>()
                                    .join(if cfg!(windows) { ";" } else { ":" })
                    })
        }
        _ => false,
    };
    if valid {
        record(
            db,
            StoredLaunch {
                installation: Installation {
                    software: Some(software),
                    version: Some(pack.minecraft_version().to_owned()),
                    loader_version,
                    build: None,
                    custom_jar: None,
                    installation_id: "created".into(),
                },
                document: document.to_owned(),
                artifacts: Vec::new(),
                warnings: Vec::new(),
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backfill_only_adopts_matching_created_launch_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().canonicalize().unwrap();
        let game = dir.join("game");
        std::fs::create_dir(&game).unwrap();
        let jar = dir.join("install/instances/test/software/vanilla.jar");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, b"jar").unwrap();
        let launch = vesta_server_install::ManagedLaunch {
            java: dir.join("java"),
            working_directory: game.clone(),
            args: vec!["-jar".into(), jar.to_str().unwrap().into(), "nogui".into()],
            required_files: vec![jar],
            artifacts: Vec::new(),
        };
        let document = crate::host_software::managed_launch_document(&launch, &[], &dir).unwrap();
        let text=crate::onboarding::provisioning::host_document_toml(serde_json::json!({"schema-version":1,"paths":{"state":"private/state","game":"game","scan-roots":["game"],"launch-definition":"private/launch.toml"},"pack":{"name":"Test","version":"initial","revision":1,"minimum-revision":1,"minecraft-version":"26.3"},"pack-access":{"mode":"minecraft"},"listener":{"mode":"disabled"}}),&dir).unwrap();
        std::fs::write(dir.join("host.toml"), &text).unwrap();
        let config =
            HostConfiguration::parse_toml(text.as_bytes(), &dir, Path::new("host.toml")).unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(config.state(), Some(&game)).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let original = StoredLaunch {
            installation: Installation {
                software: None,
                version: None,
                loader_version: None,
                build: None,
                custom_jar: None,
                installation_id: "original".into(),
            },
            document: document.clone(),
            artifacts: Vec::new(),
            warnings: Vec::new(),
        };
        store::initialize(&db, &original).unwrap();
        let edited = document.replace("vanilla.jar", "external.jar");
        backfill(&config, &db, &edited).unwrap();
        assert!(
            db.read_with(|tx| -> Result<_> {
                Ok(store::state(tx.connection())?.current.installation.software)
            })
            .unwrap()
            .is_none()
        );
        backfill(&config, &db, &document).unwrap();
        backfill(&config, &db, &document).unwrap();
        let software = db
            .read_with(|tx| -> Result<_> {
                Ok(store::state(tx.connection())?.current.installation)
            })
            .unwrap();
        assert_eq!(
            software.software,
            Some(vesta_server_install::Software::Vanilla)
        );
        assert_eq!(software.version.as_deref(), Some("26.3"));
        db.close().unwrap();
    }
}
