//! Point-in-time same-account filesystem hygiene, not hostile-race confinement.
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, &'static str>;
pub fn guard(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("local_path_refused");
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Err("local_path_refused");
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if m.file_attributes() & 0x400 != 0 {
                        return Err("local_path_refused");
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("local_path_refused"),
        }
    }
    Ok(())
}
pub fn absolute(path: &Path, base: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    guard(&path)?;
    Ok(path)
}
pub fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    guard(path)?;
    let m = fs::symlink_metadata(path).map_err(|_| "local_file_unavailable")?;
    if !m.is_file() || m.len() > limit as u64 {
        return Err("local_file_refused");
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "local_file_unavailable")?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "local_file_unavailable")?;
    if bytes.len() > limit {
        return Err("local_file_refused");
    }
    Ok(bytes)
}
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    guard(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "local_destination_exists_or_unavailable")?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "local_write_failed")
}
pub fn lock(path: &Path) -> Result<File> {
    crate::uninstall::check_start(path)?;
    lock_for_uninstall(path)
}
/// The confirmed uninstall coordinator can acquire locks after its durable
/// marker excludes normal local mutation/start paths.
pub(crate) fn lock_for_uninstall(path: &Path) -> Result<File> {
    guard(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| "local_lock_unavailable")?;
    file.try_lock_exclusive()
        .map_err(|_| "local_server_busy_or_running")?;
    Ok(file)
}
pub fn eula_accepted(game: &Path) -> bool {
    let Ok(bytes) = read(&game.join("eula.txt"), 8192) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let mut value = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, setting)) = line.split_once('=') else {
            return false;
        };
        if key.trim() == "eula" {
            if value.is_some() {
                return false;
            }
            value = Some(setting.trim() == "true");
        }
    }
    value == Some(true)
}
pub fn quoted_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or("local_path_refused")?
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    serde_json::to_string(&text).map_err(|_| "local_path_refused")
}
