//! Local administrative export for a source that the service cannot read.
use super::{browse, inspection};
use crate::files::{
    copied_export::{CopiedExport, CopyLimits},
    FileError, require_space,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// Copies into an empty, private directory supplied by the local caller.
/// The caller publishes the directory only after the verified copy succeeds.
pub fn copy(source: &Path, target: &Path, protected: Vec<PathBuf>) -> Result<(), String> {
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
    let snapshot = export.snapshot().map_err(|e| copy_error("inspect the source files", e))?;
    require_space(target, snapshot.bytes).map_err(|e| copy_error("check space for the prepared copy", e))?;
    export
        .copy_to_directory(&snapshot, target, |_, _| Ok(()))
        .map_err(|e| copy_error("copy the source files", e))?;
    export
        .preserve_modified_times(&snapshot, target)
        .map_err(|e| copy_error("preserve file timestamps", e))?;
    stopped()
}

fn copy_error(stage: &str, error: FileError) -> String {
    let detail = match error {
        FileError::LinkedPath => "The source contains a symbolic link, hard link, or mount point. Prepare a separate folder with regular files and try that folder.",
        FileError::ProtectedPath => "The source contains a protected configuration, database, key, or certificate. Prepare a separate clean Minecraft folder.",
        FileError::InvalidPath => "A file name in the source is unsupported. Check the source for names that cannot be copied.",
        FileError::NotRegular => "The source contains a special file, such as a socket or pipe. Prepare a separate folder with regular files.",
        FileError::TooLarge => "The source exceeds the import size or file count limit.",
        FileError::InsufficientSpace => "Free space on the filesystem that holds /srv/vesta-imports.",
        FileError::Conflict(_) | FileError::Corrupt => "A file changed during inspection or copy. Stop the server and any process writing to the directory, then retry.",
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
        os::unix::fs::symlink,
        time::{Duration, SystemTime},
    };

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
