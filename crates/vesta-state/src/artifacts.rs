//! A flat, owner-only artifact directory inside protected state.
//!
//! Primitives never replace an existing name: creation is create-new and publication is
//! a no-replace hard link. Callers decide naming and content verification; this module
//! confers no authority and does not interpret bytes. As with the database files, path
//! checks are same-account hygiene. An ancestor can still be raced between inspection
//! and use, so readers must verify content (for example, content-addressed digests).
use crate::{
    Result, StateDirectory, StateError,
    directory::{
        create_real_directories, is_link_or_special, open_regular, reject_linked_directories,
        require_directory, require_regular_or_absent, tighten_open_file,
    },
};
use std::{
    fmt, fs,
    fs::File,
    io,
    path::{Path, PathBuf},
};

pub const ARTIFACTS_DIR: &str = "artifacts";
pub const TRASH_DIR: &str = "trash";
pub const UPLOADS_DIR: &str = "uploads";
pub const SPOOL_DIR: &str = "spool";
pub const SUPERVISOR_DIR: &str = "supervisor";
pub const MAX_ARTIFACT_NAME_BYTES: usize = 96;

/// Flat lowercase ASCII names: alphanumerics, `-` and `.`, starting and ending with an
/// alphanumeric and never containing `..`. No separators, drive, device or stream syntax.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactName(String);
impl ArtifactName {
    pub fn new(name: &str) -> Result<Self> {
        let bytes = name.as_bytes();
        let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        if bytes.is_empty()
            || bytes.len() > MAX_ARTIFACT_NAME_BYTES
            || !bytes.first().is_some_and(edge)
            || !bytes.last().is_some_and(edge)
            || name.contains("..")
            || !bytes
                .iter()
                .all(|byte| edge(byte) || matches!(byte, b'-' | b'.'))
        {
            return Err(StateError::new("state_artifact_name_invalid"));
        }
        Ok(Self(name.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for ArtifactName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArtifactName[<redacted>]")
    }
}

/// Handle to `<state>/artifacts`. Each primitive re-verifies the state root spelling and
/// that the artifact directory is a plain, non-reparse directory before touching names.
#[derive(Clone)]
pub struct ArtifactDirectory {
    state: StateDirectory,
    path: PathBuf,
}
impl fmt::Debug for ArtifactDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArtifactDirectory[<redacted>]")
    }
}

impl StateDirectory {
    /// Create (owner-only) or verify the artifact directory. Linked/special entries refuse.
    pub fn artifacts(&self) -> Result<ArtifactDirectory> {
        self.flat_directory(ARTIFACTS_DIR)
    }

    /// Owner-only release binary directory with the same single-link/no-reparse
    /// opened-file checks as other state artifacts. Does not validate signatures.
    pub fn release_binaries(&self) -> Result<ArtifactDirectory> {
        let directory = ArtifactDirectory {
            state: self.clone(),
            path: self.path().join("bin"),
        };
        directory.verify()?;
        Ok(directory)
    }

    /// The owner-only flat directory holding recoverable copies of replaced/deleted files.
    pub fn trash(&self) -> Result<ArtifactDirectory> {
        self.flat_directory(TRASH_DIR)
    }

    /// Owner-only resumable upload staging, never inside the game root.
    pub fn uploads(&self) -> Result<ArtifactDirectory> {
        self.flat_directory(UPLOADS_DIR)
    }

    /// Owner-only, bounded cache of derived read-only views (e.g. decompressed logs).
    /// Contents are disposable; callers bound their own count and size.
    pub fn spool(&self) -> Result<ArtifactDirectory> {
        self.flat_directory(SPOOL_DIR)
    }

    /// Verified pack payloads retained by durable reviews and pending file effects.
    pub fn pack_payloads(&self) -> Result<ArtifactDirectory> {
        self.flat_directory("pack-payloads")
    }

    /// Owner-only supervisor records (process generation, lock). Never game-readable.
    pub fn supervisor(&self) -> Result<ArtifactDirectory> {
        self.flat_directory(SUPERVISOR_DIR)
    }

    fn flat_directory(&self, name: &str) -> Result<ArtifactDirectory> {
        self.verify_root_spelling()?;
        require_directory(self.path())?;
        let path = self.path().join(name);
        reject_linked_directories(&path, "state_artifact_linked")?;
        create_real_directories(&path)?;
        let directory = ArtifactDirectory {
            state: self.clone(),
            path,
        };
        directory.verify()?;
        Ok(directory)
    }
}

impl ArtifactDirectory {
    fn verify(&self) -> Result<()> {
        self.state.verify_root_spelling()?;
        reject_linked_directories(&self.path, "state_artifact_linked")?;
        require_directory(&self.path).map_err(|_| StateError::new("state_artifact_linked"))
    }

    fn entry(&self, name: &ArtifactName) -> PathBuf {
        self.path.join(name.as_str())
    }

    /// Create a new owner-only regular file opened read/write. An existing name, of any
    /// kind, refuses with `state_artifact_exists`; nothing is truncated or replaced.
    /// Drop the returned handle before linking or removing the name.
    pub fn create_new(&self, name: &ArtifactName) -> Result<File> {
        self.verify()?;
        let path = self.entry(name);
        if fs::symlink_metadata(&path).is_ok() {
            return Err(StateError::new("state_artifact_exists"));
        }
        let file = open_regular(&path, true, false)?;
        tighten_open_file(&file)?;
        Ok(file)
    }

    /// Open, or create owner-only, a long-lived regular file such as a lock file. Linked or
    /// special entries refuse. Existing bytes are kept; callers own locking semantics.
    pub fn open_or_create(&self, name: &ArtifactName) -> Result<File> {
        self.verify()?;
        let path = self.entry(name);
        if !require_regular_or_absent(&path)? {
            let file = open_regular(&path, true, false)?;
            tighten_open_file(&file)?;
            return Ok(file);
        }
        open_regular(&path, false, false).map_err(|_| StateError::new("state_artifact_linked"))
    }

    /// Add `published` as a second name for the regular file `staged`. Never replaces:
    /// an existing `published` name refuses with `state_artifact_exists`.
    pub fn link_new(&self, staged: &ArtifactName, published: &ArtifactName) -> Result<()> {
        self.verify()?;
        let source = self.entry(staged);
        if !require_regular_or_absent(&source)? {
            return Err(StateError::new("state_artifact_missing"));
        }
        let target = self.entry(published);
        match fs::hard_link(&source, &target) {
            Ok(()) => sync_directory(&self.path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(StateError::new("state_artifact_exists"))
            }
            Err(_) => Err(StateError::new("state_io")),
        }
    }

    /// Remove one plain file name. A hard-linked name is allowed (that is how a staged
    /// name is retired after publication); directories, reparse points and specials refuse.
    /// Returns `false` when the name is already absent.
    pub fn remove(&self, name: &ArtifactName) -> Result<bool> {
        self.verify()?;
        let path = self.entry(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !is_link_or_special(&metadata) && metadata.is_file() => {}
            Ok(_) => return Err(StateError::new("state_artifact_linked")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(StateError::new("state_io")),
        }
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.path).map(|()| true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(StateError::new("state_io")),
        }
    }

    /// Open an existing single-link regular file. Absent names return `None`; a name with
    /// a second link, a reparse point or a non-file refuses. Content is not verified here.
    pub fn open(&self, name: &ArtifactName) -> Result<Option<File>> {
        self.verify()?;
        let path = self.entry(name);
        match require_regular_or_absent(&path) {
            Ok(false) => Ok(None),
            Ok(true) => open_regular(&path, false, false)
                .map(Some)
                .map_err(|_| StateError::new("state_artifact_linked")),
            Err(_) => Err(StateError::new("state_artifact_linked")),
        }
    }

    /// Flush directory metadata after a synced create-new file, before recording its name.
    /// Windows has no portable directory fsync; see the platform durability limitation below.
    pub fn sync(&self) -> Result<()> {
        self.verify()?;
        sync_directory(&self.path)
    }

    /// Count entries, scanning at most `limit + 1`. Returns `None` when the limit is exceeded.
    pub fn count(&self, limit: usize) -> Result<Option<usize>> {
        self.verify()?;
        let mut count = 0usize;
        for entry in fs::read_dir(&self.path).map_err(|_| StateError::new("state_io"))? {
            entry.map_err(|_| StateError::new("state_io"))?;
            count += 1;
            if count > limit {
                return Ok(None);
            }
        }
        Ok(Some(count))
    }

    /// Valid artifact names among at most `limit` scanned entries, sorted. Foreign or
    /// non-UTF-8 names are skipped (and never touched) but still consume the scan budget.
    pub fn list(&self, limit: usize) -> Result<Vec<ArtifactName>> {
        self.verify()?;
        let mut names = Vec::new();
        let entries = fs::read_dir(&self.path).map_err(|_| StateError::new("state_io"))?;
        for entry in entries.take(limit) {
            let entry = entry.map_err(|_| StateError::new("state_io"))?;
            if let Some(name) = entry.file_name().to_str()
                && let Ok(name) = ArtifactName::new(name)
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(target_os = "linux")]
fn sync_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| StateError::new("state_io"))
}
/// Windows exposes no portable directory fsync; NTFS journals the metadata change.
/// This is a durability limitation for power loss, not a correctness claim.
#[cfg(not(target_os = "linux"))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ArtifactName;

    #[test]
    fn names_are_flat_lowercase_and_never_traverse() {
        for valid in [
            r"ab",
            "0",
            "stage-0123abcd.tmp",
            &format!("{}.vestapack", "ab".repeat(32)),
            &"a".repeat(super::MAX_ARTIFACT_NAME_BYTES),
        ] {
            assert_eq!(ArtifactName::new(valid).unwrap().as_str(), valid);
        }
        for invalid in [
            "",
            ".",
            "..",
            ".hidden",
            "trailing.",
            "trailing-",
            "-leading",
            "a..b",
            "a/b",
            r"a\b",
            "c:x",
            "A",
            "name.ZIP",
            "file:stream",
            "with space",
            "caf\u{e9}",
            &"a".repeat(super::MAX_ARTIFACT_NAME_BYTES + 1),
        ] {
            let error = ArtifactName::new(invalid).unwrap_err();
            assert_eq!(error.code(), "state_artifact_name_invalid", "{invalid:?}");
        }
        assert_eq!(
            format!("{:?}", ArtifactName::new("secret-name").unwrap()),
            "ArtifactName[<redacted>]"
        );
    }
}
