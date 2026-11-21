//! Publish a root-verified copy that the dedicated Vesta account can read.
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
};

const EXPORTS: &str = "/srv/vesta-imports";

pub fn run(args: &[OsString]) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("Run this command with sudo.".into());
    }
    let [action, source] = args else {
        return Err("Usage: sudo vesta import prepare <absolute Minecraft server path>".into());
    };
    let source = Path::new(source);
    if action != "prepare" || !source.is_absolute() {
        return Err("Usage: sudo vesta import prepare <absolute Minecraft server path>".into());
    }
    let installed = fs::read_to_string(crate::linux_install::DATA_ROOT_RECORD)
        .map_err(|_| "Install Vesta Server before preparing an import.")?;
    let installed = PathBuf::from(installed.trim());
    if !installed.is_absolute() {
        return Err("Invalid Vesta installation data path.".into());
    }
    let group = unsafe {
        libc::getpwnam(c"vesta".as_ptr())
            .as_ref()
            .map(|account| account.pw_gid)
            .ok_or("Vesta service account is missing.")?
    };
    let path = prepare(source, Path::new(EXPORTS), group, installed)?;
    println!("Prepared import: {}", path.display());
    println!(
        "Open this path in the launcher’s import picker. Keep the copy until the import finishes."
    );
    println!(
        "The prepared copy can be deleted afterward from: {}",
        path.display()
    );
    Ok(())
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

fn controlled(path: &Path) -> std::io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(std::io::Error::other(
            "Import staging requires a root-controlled directory",
        ));
    }
    Ok(metadata)
}

fn publish_modes(path: &Path, group: u32) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let mode = if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            publish_modes(&entry?.path(), group)?;
        }
        0o750
    } else if metadata.is_file() {
        0o640
    } else {
        return Err(std::io::Error::other("Unexpected file in prepared import"));
    };
    chown(path, Some(0), Some(group))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn prepare(
    source: &Path,
    staging: &Path,
    group: u32,
    installed: PathBuf,
) -> Result<PathBuf, String> {
    let name = source
        .file_name()
        .ok_or("Choose a Minecraft server folder.")?;
    controlled(staging.parent().ok_or("Invalid staging path.")?).map_err(|e| e.to_string())?;
    match private_dir(staging) {
        Ok(()) => {
            chown(staging, Some(0), Some(group)).map_err(|e| e.to_string())?;
            fs::set_permissions(staging, fs::Permissions::from_mode(0o750))
                .map_err(|e| e.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = controlled(staging).map_err(|e| e.to_string())?;
            if metadata.gid() != group || metadata.mode() & 0o777 != 0o750 {
                return Err(
                    "Use root:vesta ownership and mode 0750 for the import staging directory."
                        .into(),
                );
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    let id = uuid::Uuid::new_v4().to_string();
    let pending = staging.join(format!(".{id}.pending"));
    let ready = staging.join(id);
    private_dir(&pending).map_err(|e| e.to_string())?;
    let result = (|| {
        let target = pending.join(name);
        private_dir(&target).map_err(|e| e.to_string())?;
        vesta_control::machine::imports::prepare::copy(
            source,
            &target,
            vec![
                installed,
                staging.into(),
                "/usr/local/lib/vesta".into(),
                "/etc".into(),
                "/root".into(),
            ],
        )?;
        publish_modes(&pending, group).map_err(|e| e.to_string())?;
        fs::rename(&pending, &ready).map_err(|e| e.to_string())?;
        Ok(ready.join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    result
}
