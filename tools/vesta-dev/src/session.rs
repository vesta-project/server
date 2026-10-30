//! Reusable development staging, scoped to one checkout. No runtime data is deleted.
use super::{BINARIES, executable};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub fn open(repo: &Path, selected: Option<PathBuf>, persistent: bool) -> Result<PathBuf, String> {
    let explicit = selected.is_some();
    let directory = selected.unwrap_or_else(|| {
        let name = if persistent {
            format!(
                "checkout-{:x}",
                Sha256::digest(repo.as_os_str().as_encoded_bytes())
            )
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        std::env::temp_dir().join("vesta-dev-builds").join(name)
    });
    let marker = directory.join(".vesta-dev-session");
    if directory.exists() {
        super::restart::guarded(&marker)?;
        if fs::read(&marker).map_err(|_| "development session marker is unavailable")?
            != repo.as_os_str().as_encoded_bytes()
        {
            return Err("development session belongs to another checkout".into());
        }
    } else {
        if explicit {
            return Err("selected development session does not exist".into());
        }
        let state =
            vesta_state::StateDirectory::new(&directory, None).map_err(|e| e.code().to_owned())?;
        state.prepare().map_err(|e| e.code().to_owned())?;
        fs::write(&marker, repo.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        state
            .tighten_file(&marker)
            .map_err(|e| e.code().to_owned())?;
    }
    Ok(directory)
}

pub fn update_lock(session: &Path) -> Result<File, String> {
    let path = session.join("update.lock");
    if path.exists() {
        super::restart::guarded(&path)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let until = Instant::now() + Duration::from_secs(300);
    let mut announced = false;
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                if !announced {
                    println!("Another update for this checkout is finishing; waiting...");
                    announced = true;
                }
                if Instant::now() >= until {
                    return Err(
                        "another development update is still running; retry when it finishes"
                            .into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("cannot lock the development update: {e}")),
        }
    }
}

fn same_file(first: &Path, second: &Path) -> std::io::Result<bool> {
    let mut first = File::open(first)?;
    let mut second = File::open(second)?;
    if first.metadata()?.len() != second.metadata()?.len() {
        return Ok(false);
    }
    let mut a = [0_u8; 65_536];
    let mut b = [0_u8; 65_536];
    loop {
        let length = first.read(&mut a)?;
        second.read_exact(&mut b[..length])?;
        if a[..length] != b[..length] {
            return Ok(false);
        }
        if length == 0 {
            return Ok(true);
        }
    }
}

/** Reuse only a complete byte-identical build under this session's own builds directory. */
pub fn unchanged_build(session: &Path, artifacts: &BTreeMap<String, PathBuf>) -> Option<PathBuf> {
    let pointer = session.join("current-bin");
    super::restart::guarded(&pointer).ok()?;
    let mut text = String::new();
    File::open(pointer)
        .ok()?
        .take(4097)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() > 4096 {
        return None;
    }
    let bin = PathBuf::from(text);
    if bin.parent()? != session.join("builds") {
        return None;
    }
    for name in BINARIES {
        let candidate = bin.join(executable(name));
        super::restart::guarded(&candidate).ok()?;
        if !same_file(artifacts.get(name)?, &candidate).ok()? {
            return None;
        }
    }
    Some(bin)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_sessions_are_checkout_bound_and_missing_sessions_are_not_created() {
        let temp = tempfile::tempdir().unwrap();
        let selected = temp.path().join("session");
        assert!(open(temp.path(), Some(selected.clone()), false).is_err());
        assert!(!selected.exists());
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join(".vesta-dev-session"), b"another checkout").unwrap();
        assert!(open(temp.path(), Some(selected.clone()), false).is_err());
        fs::write(
            selected.join(".vesta-dev-session"),
            temp.path().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(
            open(temp.path(), Some(selected.clone()), false).unwrap(),
            selected
        );
        let lock = update_lock(&selected).unwrap();
        assert!(
            FileExt::try_lock_exclusive(&File::open(selected.join("update.lock")).unwrap())
                .is_err()
        );
        drop(lock);
        assert!(update_lock(&selected).is_ok());
    }
}
