//! Object storage for the backup repository: a flat key → bytes namespace.
//!
//! Keys are short ASCII paths built by the engine (`config.json`, `chunks/ab/<hash>`,
//! `snapshots/<id>`, `tags/<id>`), never caller input. Implementations: a local
//! directory (`LocalStore`) and S3-compatible object storage (`s3::S3Store`).
//! Writes of a key are atomic: readers see the old bytes or the new bytes, never a
//! partial object. Nothing here interprets contents.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    NotFound,
    /// Storage refused or failed the request (I/O, network, HTTP status).
    Unavailable,
    /// A key or listing violated the engine's own rules (corrupt repository).
    Invalid,
    TooLarge,
}
impl StoreError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotFound => "backup_object_missing",
            Self::Unavailable => "backup_storage_unavailable",
            Self::Invalid => "backup_repository_invalid",
            Self::TooLarge => "backup_object_too_large",
        }
    }
}
pub type Result<T> = std::result::Result<T, StoreError>;

/// Largest single object the engine ever writes or reads (a compressed chunk or a
/// compressed snapshot manifest).
pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    /// Last modification, Unix milliseconds (used for prune grace periods).
    pub modified_ms: i64,
}

pub trait Store: Send + Sync {
    /// Atomic create-or-replace.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()>;
    /// Bounded read of the whole object.
    fn get(&self, key: &str) -> Result<Vec<u8>>;
    fn exists(&self, key: &str) -> Result<bool>;
    /// All objects whose key starts with `prefix`, in any order. Bounded by the caller's
    /// `limit`: more than `limit` objects is `TooLarge`.
    fn list(&self, prefix: &str, limit: usize) -> Result<Vec<ObjectInfo>>;
    /// Removing a missing key succeeds.
    fn delete(&self, key: &str) -> Result<()>;
}

/// Engine keys: lowercase ASCII letters, digits, `.`, `-`, `_` and `/` separators,
/// no empty, `.` or `..` segments, at most 200 bytes.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 200
        && key.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        })
}

/// A repository in a local (or mounted) directory.
pub struct LocalStore {
    root: PathBuf,
}
impl std::fmt::Debug for LocalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalStore")
    }
}
impl LocalStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn path(&self, key: &str) -> Result<PathBuf> {
        if !valid_key(key) {
            return Err(StoreError::Invalid);
        }
        Ok(key.split('/').fold(self.root.clone(), |p, s| p.join(s)))
    }
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Refuse links and special files anywhere the store touches.
fn plain_file(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(meta)),
        Ok(_) => Err(StoreError::Invalid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(StoreError::Unavailable),
    }
}

impl Store for LocalStore {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_OBJECT_BYTES {
            return Err(StoreError::TooLarge);
        }
        let path = self.path(key)?;
        let parent = path.parent().ok_or(StoreError::Invalid)?;
        fs::create_dir_all(parent).map_err(|_| StoreError::Unavailable)?;
        // Write a unique temporary sibling, sync it, then rename over the key.
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Unavailable)?;
        let temp = parent.join(format!(
            ".tmp-{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
            return Err(StoreError::Unavailable);
        }
        #[cfg(unix)]
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.path(key)?;
        let meta = plain_file(&path)?.ok_or(StoreError::NotFound)?;
        if meta.len() > MAX_OBJECT_BYTES as u64 {
            return Err(StoreError::TooLarge);
        }
        let bytes = fs::read(&path).map_err(|_| StoreError::Unavailable)?;
        if bytes.len() > MAX_OBJECT_BYTES {
            return Err(StoreError::TooLarge);
        }
        Ok(bytes)
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(plain_file(&self.path(key)?)?.is_some())
    }

    fn list(&self, prefix: &str, limit: usize) -> Result<Vec<ObjectInfo>> {
        // Prefixes are a directory path ending in '/', or empty for everything.
        let directory = match prefix.strip_suffix('/') {
            Some(dir) => self.path(dir)?,
            None if prefix.is_empty() => self.root.clone(),
            None => return Err(StoreError::Invalid),
        };
        let mut found = Vec::new();
        let mut stack = vec![(directory, prefix.to_owned())];
        while let Some((dir, key_prefix)) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(StoreError::Unavailable),
            };
            for entry in entries {
                let entry = entry.map_err(|_| StoreError::Unavailable)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(".tmp-") {
                    continue;
                }
                let meta = entry
                    .path()
                    .symlink_metadata()
                    .map_err(|_| StoreError::Unavailable)?;
                let key = format!("{key_prefix}{name}");
                if meta.is_dir() {
                    stack.push((entry.path(), format!("{key}/")));
                } else if meta.is_file() && valid_key(&key) {
                    if found.len() >= limit {
                        return Err(StoreError::TooLarge);
                    }
                    found.push(ObjectInfo {
                        key,
                        size: meta.len(),
                        modified_ms: meta.modified().map(millis).unwrap_or(0),
                    });
                }
            }
        }
        Ok(found)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.path(key)?;
        match plain_file(&path)? {
            None => Ok(()),
            Some(_) => fs::remove_file(&path).map_err(|_| StoreError::Unavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_strict() {
        assert!(valid_key("chunks/ab/abcdef"));
        assert!(valid_key("config.json"));
        for key in ["", "/x", "x/", "a//b", "../x", "a/./b", "A", "a b", "a\\b"] {
            assert!(!valid_key(key), "{key}");
        }
    }

    #[test]
    fn local_store_round_trips_lists_and_replaces_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let store = LocalStore::new(temp.path().join("repo"));
        assert_eq!(store.get("config.json"), Err(StoreError::NotFound));
        store.put("config.json", b"one").unwrap();
        store.put("config.json", b"two").unwrap();
        assert_eq!(store.get("config.json").unwrap(), b"two");
        store.put("chunks/ab/abc", b"c").unwrap();
        store.put("chunks/cd/cde", b"d").unwrap();
        let mut keys: Vec<_> = store
            .list("chunks/", 10)
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        keys.sort();
        assert_eq!(keys, ["chunks/ab/abc", "chunks/cd/cde"]);
        assert_eq!(store.list("chunks/", 1), Err(StoreError::TooLarge));
        store.delete("chunks/ab/abc").unwrap();
        store.delete("chunks/ab/abc").unwrap();
        assert!(!store.exists("chunks/ab/abc").unwrap());
        assert_eq!(store.put("../escape", b"x"), Err(StoreError::Invalid));
    }
}
