//! Publish a root-verified copy that the dedicated Vesta account can read.
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    process::Command,
};

pub fn run(args: &[OsString]) -> Result<(), String> {
    if account_id(&["-u"])? != 0 {
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
    let group = account_id(&["-g", "vesta"]).map_err(|_| "Vesta service account is missing.")?;
    let user = account_id(&["-u", "vesta"]).map_err(|_| "Vesta service account is missing.")?;
    let staging = installed.join(vesta_control::machine::imports::prepare::PREPARED_DIRECTORY);
    let path = prepare(source, &staging, user, group, installed)?;
    println!("Prepared import: {}", path.display());
    println!(
        "Open this path in the launcher’s import picker. Keep the copy until the import finishes."
    );
    println!(
        "A same-filesystem import moves this prepared copy into managed storage. If import copies it instead, the prepared folder can be deleted afterward from: {}",
        path.display()
    );
    Ok(())
}

fn account_id(args: &[&str]) -> Result<u32, String> {
    let output = Command::new("/usr/bin/id")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Could not look up the local account.".into());
    }
    std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|id| id.trim().parse().ok())
        .ok_or_else(|| "Invalid local account ID.".into())
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

fn publish_directories(path: &Path, user: u32, group: u32) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            publish_directories(&entry.path(), user, group)?;
        } else if !kind.is_file() {
            return Err(std::io::Error::other("Unexpected file in prepared import"));
        }
    }
    chown(path, Some(user), Some(group))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    fs::File::open(path)?.sync_all()
}

fn prepare(
    source: &Path,
    staging: &Path,
    user: u32,
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
        vesta_control::machine::imports::prepare::copy_for_service(
            source,
            &target,
            vec![
                installed,
                staging.into(),
                "/usr/local/lib/vesta".into(),
                "/etc".into(),
                "/root".into(),
            ],
            user,
            group,
        )?;
        publish_directories(&pending, user, group).map_err(|e| e.to_string())?;
        fs::rename(&pending, &ready).map_err(|e| e.to_string())?;
        fs::File::open(staging)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(ready.join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    result
}
