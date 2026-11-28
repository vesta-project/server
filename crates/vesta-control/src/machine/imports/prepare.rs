//! Local administrative export for a source that the service cannot read.
use super::{browse, inspection};
use crate::files::{
    FileError,
    copied_export::{CopiedExport, CopyLimits},
    require_space,
};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, fchown},
    path::{Path, PathBuf},
};

/// Under the Host data directory, within the service's writable mount.
pub const PREPARED_DIRECTORY: &str = "prepared-imports";

#[allow(unsafe_code)]
pub(super) fn service_uid() -> u32 {
    // SAFETY: geteuid takes no arguments and has no memory effects.
    unsafe { libc::geteuid() }
}

/// The administrator controls the root. The CLI publishes one private,
/// service-owned wrapper with a UUID name and puts the game directory in it.
pub(super) fn is_prepared(source: &Path, root: &Path, root_owner: u32) -> bool {
    let Some(wrapper) = source.parent() else {
        return false;
    };
    wrapper.parent() == Some(root)
        && trusted_root(root, root_owner)
        && prepared_wrapper(wrapper)
        && owned_directory(source, service_uid(), 0o077)
}

pub(super) fn prepared_wrapper(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.len() == 36 && uuid::Uuid::parse_str(name).is_ok())
        && owned_directory(path, service_uid(), 0o077)
}

pub(super) fn trusted_root(root: &Path, owner: u32) -> bool {
    root.parent()
        .is_some_and(|parent| owned_directory(parent, owner, 0o022))
        && owned_directory(root, owner, 0o022)
}

fn owned_directory(path: &Path, uid: u32, forbidden_mode: u32) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_dir() && meta.uid() == uid && meta.mode() & forbidden_mode == 0)
}

/// Copies into an empty, private directory supplied by the local caller.
/// The caller publishes the directory only after the verified copy succeeds.
pub fn copy(source: &Path, target: &Path, protected: Vec<PathBuf>) -> Result<(), String> {
    copy_owned(source, target, protected, None)
}

/// The local setup command assigns file ownership before the batched data flush.
pub fn copy_for_service(
    source: &Path,
    target: &Path,
    protected: Vec<PathBuf>,
    user: u32,
    group: u32,
) -> Result<(), String> {
    copy_owned(source, target, protected, Some((user, group)))
}

fn copy_owned(
    source: &Path,
    target: &Path,
    protected: Vec<PathBuf>,
    owner: Option<(u32, u32)>,
) -> Result<(), String> {
    let browser = browse::FolderBrowser::new(vec![], protected).map_err(|e| e.to_string())?;
    let canonical = source.canonicalize().map_err(|e| e.to_string())?;
    if source != canonical
        || browse::system_tree(&canonical)
        || browser.protected_overlap(&canonical)
    {
        return Err(
            "Choose a real Minecraft folder outside protected Vesta and system paths.".into(),
        );
    }
    let export = CopiedExport::open(&canonical, &[target])
        .map_err(|e| copy_error("open the source", e))?
        .with_limits(CopyLimits::IMPORT);
    let properties = inspection::read_properties(&canonical.join("server.properties"))
        .map_err(|_| "Choose the Minecraft server folder containing server.properties.")?;
    let port = properties
        .get("server-port")
        .and_then(|port| port.parse().ok());
    let stopped = || {
        if inspection::stopped_evidence(&canonical, port, &BTreeSet::new()).is_some() {
            return Err("The source appears active. Stop it, turn off auto-restart, and try again after one minute.".into());
        }
        Ok(())
    };
    stopped()?;
    let snapshot = export
        .snapshot()
        .map_err(|e| copy_error("inspect the source files", e))?;
    require_space(target, snapshot.bytes)
        .map_err(|e| copy_error("check space for the prepared copy", e))?;
    export
        .copy_to_directory_with(
            &snapshot,
            target,
            |_, _| Ok(()),
            |original, copy| {
                copy.set_modified(original.metadata()?.modified()?)?;
                if let Some((uid, gid)) = owner {
                    copy.set_permissions(fs::Permissions::from_mode(0o600))?;
                    fchown(copy, Some(uid), Some(gid))?;
                }
                Ok(())
            },
        )
        .map_err(|e| copy_error("copy the source files", e))?;
    stopped()
}

fn copy_error(stage: &str, error: FileError) -> String {
    let detail = match error {
        FileError::LinkedPath => {
            "The source contains a symbolic link, hard link, or mount point. Prepare a separate folder with regular files and try that folder."
        }
        FileError::ProtectedPath => {
            "The source contains a protected configuration, database, key, or certificate. Prepare a separate clean Minecraft folder."
        }
        FileError::InvalidPath => {
            "A file name in the source is unsupported. Check the source for names that cannot be copied."
        }
        FileError::NotRegular => {
            "The source contains a special file, such as a socket or pipe. Prepare a separate folder with regular files."
        }
        FileError::TooLarge => "The source exceeds the import size or file count limit.",
        FileError::InsufficientSpace => {
            "Free space on the filesystem that holds the prepared imports."
        }
        FileError::Conflict(_) | FileError::Corrupt => {
            "A file changed during inspection or copy. Stop the server and any process writing to the directory, then retry."
        }
        _ => "Check that the source remains readable and that the staging filesystem is writable.",
    };
    format!("Could not {stage} ({}). {detail}", error.code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        net::TcpListener,
        os::unix::fs::{PermissionsExt, symlink},
        time::{Duration, SystemTime},
    };

    #[test]
    fn only_private_service_owned_copies_in_the_prepared_layout_are_movable() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("prepared");
        let wrapper = root.join(uuid::Uuid::new_v4().to_string());
        let source = wrapper.join("game");
        fs::create_dir_all(&source).unwrap();
        let uid = service_uid();
        assert!(!is_prepared(&source, &root, uid));
        for dir in [&wrapper, &source] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(is_prepared(&source, &root, uid));
        assert!(!is_prepared(&source, &root, uid.wrapping_add(1)));
        assert!(!is_prepared(&source, parent.path(), uid));
        assert!(!is_prepared(&wrapper, &root, uid));
    }

    #[test]
    fn staging_copies_a_stopped_server_and_preserves_activity_timestamps() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        fs::write(
            source.path().join("server.properties"),
            format!("server-port={port}\n"),
        )
        .unwrap();
        assert!(
            copy(source.path(), target.path(), vec![])
                .unwrap_err()
                .contains("source appears active")
        );
        assert!(fs::read_dir(target.path()).unwrap().next().is_none());
        drop(listener);
        let world = source.path().join("world/region");
        fs::create_dir_all(&world).unwrap();
        let file = world.join("r.0.0.mca");
        fs::write(&file, b"world data").unwrap();
        fs::File::open(&file)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        copy(source.path(), target.path(), vec![]).unwrap();
        let staged = target.path().join("world/region/r.0.0.mca");
        assert_eq!(fs::read(&staged).unwrap(), b"world data");
        assert_eq!(
            fs::metadata(staged).unwrap().modified().unwrap(),
            fs::metadata(file).unwrap().modified().unwrap()
        );
    }

    #[test]
    fn plugin_keys_copy_but_vesta_secrets_at_the_top_do_not() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        fs::write(
            source.path().join("server.properties"),
            format!(
                "server-port={port}
"
            ),
        )
        .unwrap();
        // Floodgate keeps its encryption key as plugins/floodgate/key.pem.
        fs::create_dir_all(source.path().join("plugins/floodgate")).unwrap();
        fs::write(
            source.path().join("plugins/floodgate/key.pem"),
            b"floodgate",
        )
        .unwrap();
        copy(source.path(), target.path(), vec![]).unwrap();
        assert_eq!(
            fs::read(target.path().join("plugins/floodgate/key.pem")).unwrap(),
            b"floodgate"
        );

        let vesta = tempfile::tempdir().unwrap();
        let empty = tempfile::tempdir().unwrap();
        fs::write(
            vesta.path().join("server.properties"),
            format!(
                "server-port={port}
"
            ),
        )
        .unwrap();
        fs::write(vesta.path().join("key.pem"), b"vesta").unwrap();
        assert!(
            copy(vesta.path(), empty.path(), vec![])
                .unwrap_err()
                .contains("file_path_protected")
        );
    }

    #[test]
    fn staging_rejects_protected_sources_and_linked_files() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        assert!(copy(source.path(), target.path(), vec![source.path().into()]).is_err());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        fs::write(
            source.path().join("server.properties"),
            format!("server-port={port}\n"),
        )
        .unwrap();
        symlink("/etc/passwd", source.path().join("linked")).unwrap();
        let error = copy(source.path(), target.path(), vec![]).unwrap_err();
        assert!(error.contains("inspect the source files"));
        assert!(error.contains("file_path_linked"));
        assert!(fs::read_dir(target.path()).unwrap().next().is_none());
    }
}
