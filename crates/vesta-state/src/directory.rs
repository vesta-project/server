use crate::{Result, StateError};
use fs2::FileExt;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::{
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

pub const DATABASE_FILE: &str = "vesta-state.db";
pub const BACKUPS_DIR: &str = "migration-backups";
pub const RUNNER_LOCK_FILE: &str = "operation-runner.lock";

/// Protected local state outside the game-visible tree, in both directions.
///
/// Windows and Linux are supported; unsupported OS/filesystems fail closed. Linux additionally
/// requires a recognized disk filesystem from mountinfo (not tmpfs, overlay, or network mounts).
/// This is stricter than the Java directory wrapper. Neither mount metadata nor an owner-only
/// ACL proves physical locality, confinement, or protection from the service account itself.
#[derive(Clone, Debug)]
pub struct StateDirectory {
    raw: PathBuf,
    game: Option<PathBuf>,
    path: PathBuf,
    database: PathBuf,
    backups: PathBuf,
}

impl StateDirectory {
    pub fn new(state: impl AsRef<Path>, game: Option<&Path>) -> Result<Self> {
        let raw = raw_absolute(state.as_ref())?;
        // Inspect the raw spelling, including ancestors canceled by '..', before normalization.
        reject_linked_directories(&raw, "state_dir_linked")?;
        let path = normalize_real(&raw)?;
        let game = game.map(raw_absolute).transpose()?;
        if let Some(game) = &game {
            refuse_overlap(&path, &normalize_real(game)?)?;
        }
        Ok(Self {
            database: path.join(DATABASE_FILE),
            backups: path.join(BACKUPS_DIR),
            raw,
            game,
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn database_file(&self) -> &Path {
        &self.database
    }

    pub fn backups_dir(&self) -> &Path {
        &self.backups
    }

    pub fn prepare(&self) -> Result<()> {
        self.verify_root_spelling()?;
        ensure_supported_filesystem(&self.path)?;
        create_real_directories(&self.path)?;
        self.verify_root_spelling()?;
        require_directory(&self.path)?;
        tighten_directory(&self.path)?;
        create_real_directories(&self.backups)?;
        require_directory(&self.backups)?;
        tighten_directory(&self.backups)?;
        self.verify_database_files()?;
        // Existing files also get tightened before SQLite can use them.
        for file in database_paths(&self.database) {
            self.tighten_file(&file)?;
        }
        for entry in
            fs::read_dir(&self.backups).map_err(|_| StateError::new("state_dir_unavailable"))?
        {
            let file = entry
                .map_err(|_| StateError::new("state_dir_unavailable"))?
                .path();
            self.tighten_file(&file)?;
        }
        Ok(())
    }

    /// Re-check all roots and companions before each SQLite connection.
    ///
    /// Residual TOCTOU: SQLite opens by name, and a same-account process may exchange an ancestor
    /// after inspection. NOFOLLOW protects the leaf on supported opens, not the complete path.
    /// Permissions, re-checks, and an idle WAL anchor reduce accidental churn, not hostile races.
    pub fn verify_database_files(&self) -> Result<()> {
        #[cfg(feature = "latency-probe")]
        let _latency_state_files_verify = crate::latency_probe::Span::new("state_files_verify");
        self.verify_root_spelling()?;
        require_directory(&self.path)?;
        require_directory(&self.backups)?;
        for file in database_paths(&self.database) {
            require_regular_or_absent(&file)?;
        }
        require_regular_or_absent(&self.path.join(RUNNER_LOCK_FILE))?;
        for entry in
            fs::read_dir(&self.backups).map_err(|_| StateError::new("state_file_linked"))?
        {
            let file = entry
                .map_err(|_| StateError::new("state_file_linked"))?
                .path();
            require_regular_or_absent(&file)?;
        }
        Ok(())
    }

    /// Tighten only a regular, non-linked file inside this state root. Absent files are harmless.
    pub fn tighten_file(&self, file: &Path) -> Result<()> {
        let raw = raw_absolute(file)?;
        let parent = raw
            .parent()
            .ok_or_else(|| StateError::new("state_file_linked"))?;
        reject_linked_directories(parent, "state_file_linked")?;
        if !is_within(&lexically_normalize(&raw), &self.path)? {
            // The stored root is canonical. An unmatched native spelling may be
            // a Windows 8.3 alias. Resolve only the inspected parent, never the
            // leaf; ordinary already-matching paths need no extra filesystem query.
            let name = raw
                .file_name()
                .ok_or_else(|| StateError::new("state_file_linked"))?;
            let admitted = normalize_real(parent)
                .map_err(|_| StateError::new("state_file_linked"))?
                .join(name);
            if !is_within(&admitted, &self.path)? {
                return Err(StateError::new("state_file_linked"));
            }
        }
        if require_regular_or_absent(&raw)? {
            let held = open_regular(&raw, false, false)?;
            tighten_open_file(&held)?;
            require_regular_or_absent(&raw)?;
        }
        Ok(())
    }

    /// OS-held advisory lock. The file is never truncated or unlinked on release.
    pub fn acquire_runner_lock(&self) -> Result<RunnerLock> {
        self.verify_database_files()?;
        let path = self.path.join(RUNNER_LOCK_FILE);
        require_regular_or_absent(&path)?;
        let file = open_regular(&path, false, true)
            .map_err(|_| StateError::new("state_runner_lock_failed"))?;
        tighten_open_file(&file).map_err(|_| StateError::new("state_runner_lock_failed"))?;
        require_regular_or_absent(&path)?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                || error.kind() == io::ErrorKind::WouldBlock
            {
                StateError::new("state_runner_lock_held")
            } else {
                StateError::new("state_runner_lock_failed")
            }
        })?;
        Ok(RunnerLock { file })
    }

    pub(crate) fn verify_root_spelling(&self) -> Result<()> {
        reject_linked_directories(&self.raw, "state_dir_linked")?;
        reject_linked_directories(&self.path, "state_dir_linked")?;
        let actual = normalize_real(&self.raw)?;
        if !is_within(&actual, &self.path)? || !is_within(&self.path, &actual)? {
            return Err(StateError::new("state_dir_invalid"));
        }
        if let Some(game) = &self.game {
            refuse_overlap(&actual, &normalize_real(game)?)?;
        }
        Ok(())
    }

    /// Reserve an empty owner-only file atomically, without ever replacing a backup/database.
    pub(crate) fn reserve_file(&self, path: &Path) -> Result<File> {
        self.verify_database_files()?;
        if !is_within(path, &self.path)? {
            return Err(StateError::new("state_file_linked"));
        }
        if let Some(parent) = path.parent() {
            reject_linked_directories(parent, "state_file_linked")?;
        }
        require_regular_or_absent(path)?;
        let file = open_regular(path, true, false)?;
        tighten_open_file(&file)?;
        require_regular_or_absent(path)?;
        Ok(file)
    }
}

/// The private file handle keeps the exclusive OS lock alive until this value is dropped.
#[derive(Debug)]
pub struct RunnerLock {
    file: File,
}

impl Drop for RunnerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        // File's Drop closes the handle, including if the explicit unlock failed.
    }
}

pub(crate) fn database_paths(database: &Path) -> [PathBuf; 4] {
    let sibling = |suffix: &str| {
        let mut name = database.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        database.with_file_name(name)
    };
    [
        database.to_owned(),
        sibling("-wal"),
        sibling("-shm"),
        sibling("-journal"),
    ]
}

pub(crate) fn require_regular_or_absent(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_link_or_special(&metadata) || !metadata.is_file() {
                return Err(StateError::new("state_file_linked"));
            }
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(StateError::new("state_file_linked"));
                }
            }
            #[cfg(windows)]
            crate::windows_security::verify_regular(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(StateError::new("state_file_linked")),
    }
}

pub(crate) fn is_link_or_special(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir())
}

pub(crate) fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| StateError::new("state_file_linked"))?;
    if is_link_or_special(&metadata) || !metadata.is_dir() {
        return Err(StateError::new("state_file_linked"));
    }
    Ok(())
}

pub(crate) fn reject_linked_directories(path: &Path, code: &'static str) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if !is_link_or_special(&metadata) && metadata.is_dir() => {}
            Ok(_) => return Err(StateError::new(code)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(StateError::new(code)),
        }
    }
    Ok(())
}

fn raw_absolute(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(StateError::new("state_dir_invalid"));
    }
    #[cfg(windows)]
    crate::windows_security::validate_path(path)?;
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()
            .map_err(|_| StateError::new("state_dir_invalid"))?
            .join(path))
    }
}

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

fn normalize_real(raw: &Path) -> Result<PathBuf> {
    let absolute = lexically_normalize(raw);
    let mut ancestor = absolute.as_path();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| StateError::new("state_dir_invalid"))?;
            }
            Err(_) => return Err(StateError::new("state_dir_invalid")),
        }
    }
    let real = fs::canonicalize(ancestor).map_err(|_| StateError::new("state_dir_invalid"))?;
    let suffix = absolute
        .strip_prefix(ancestor)
        .map_err(|_| StateError::new("state_dir_invalid"))?;
    Ok(lexically_normalize(&real.join(suffix)))
}

fn refuse_overlap(state: &Path, game: &Path) -> Result<()> {
    if is_within(state, game)? || is_within(game, state)? {
        Err(StateError::new("state_dir_overlaps_game_root"))
    } else {
        Ok(())
    }
}

fn is_within(path: &Path, root: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        crate::windows_security::is_within(path, root)
    }
    #[cfg(not(windows))]
    {
        Ok(path.starts_with(root))
    }
}

pub(crate) fn create_real_directories(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut ancestor = path;
    while !fs::symlink_metadata(ancestor)
        .map(|_| true)
        .or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(error)
            }
        })
        .map_err(|_| StateError::new("state_dir_unavailable"))?
    {
        missing.push(ancestor.to_owned());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| StateError::new("state_dir_invalid"))?;
    }
    require_directory(ancestor)?;
    for directory in missing.iter().rev() {
        reject_linked_directories(directory, "state_dir_linked")?;
        let builder = fs::DirBuilder::new();
        #[cfg(target_os = "linux")]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        match builder.create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(StateError::new("state_dir_unavailable")),
        }
        require_directory(directory)?;
        tighten_directory(directory)?;
    }
    Ok(())
}

pub(crate) fn open_regular(path: &Path, create_new: bool, create: bool) -> Result<File> {
    #[cfg(windows)]
    {
        crate::windows_security::open_regular(path, create_new, create)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .create_new(create_new)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(StateError::new("state_file_linked"));
        }
        Ok(file)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (path, create_new, create);
        Err(StateError::new("state_permissions_unsupported"))
    }
}

pub(crate) fn open_execution_file(path: &Path) -> Result<File> {
    #[cfg(windows)]
    {
        crate::windows_security::open_execution_file(path)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(StateError::new("state_file_linked"));
        }
        Ok(file)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Err(StateError::new("state_permissions_unsupported"))
    }
}

pub(crate) fn tighten_open_file(file: &File) -> Result<()> {
    #[cfg(windows)]
    {
        crate::windows_security::tighten_handle(file, false)
    }
    #[cfg(target_os = "linux")]
    {
        tighten_posix_handle(file, false)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = file;
        Err(StateError::new("state_permissions_unsupported"))
    }
}

fn tighten_directory(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        crate::windows_security::tighten_directory(path)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)
            .map_err(|_| StateError::new("state_permissions_failed"))?;
        tighten_posix_handle(&file, true)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Err(StateError::new("state_permissions_unsupported"))
    }
}

#[cfg(target_os = "linux")]
fn tighten_posix_handle(file: &File, directory: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if directory { 0o700 } else { 0o600 };
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| StateError::new("state_permissions_failed"))?;
    let metadata = file
        .metadata()
        .map_err(|_| StateError::new("state_permissions_failed"))?;
    if metadata.permissions().mode() & 0o7777 != mode
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(StateError::new("state_permissions_unsupported"));
    }
    Ok(())
}

fn ensure_supported_filesystem(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        crate::windows_security::ensure_supported_filesystem(path)
    }
    #[cfg(target_os = "linux")]
    {
        ensure_linux_disk_filesystem(path)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Err(StateError::new("state_permissions_unsupported"))
    }
}

#[cfg(target_os = "linux")]
fn ensure_linux_disk_filesystem(path: &Path) -> Result<()> {
    use std::{ffi::OsString, io::Read, os::unix::ffi::OsStringExt};
    const MAX_MOUNTINFO_BYTES: u64 = 1_048_576;
    let failure = || StateError::new("state_filesystem_unsupported");
    let mut contents = Vec::new();
    File::open("/proc/self/mountinfo")
        .map_err(|_| failure())?
        .take(MAX_MOUNTINFO_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|_| failure())?;
    if contents.len() as u64 > MAX_MOUNTINFO_BYTES {
        return Err(failure());
    }
    let mut best_depth = None;
    let mut supported = false;
    for line in contents
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let fields: Vec<_> = line.split(|byte| byte.is_ascii_whitespace()).collect();
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(failure)?;
        if separator < 6 || fields.len() < separator + 4 {
            return Err(failure());
        }
        let encoded = fields[4];
        let mut decoded = Vec::with_capacity(encoded.len());
        let mut index = 0;
        while index < encoded.len() {
            if encoded[index] == b'\\' {
                let octal = encoded.get(index + 1..index + 4).ok_or_else(failure)?;
                if !octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                    return Err(failure());
                }
                let value = ((octal[0] - b'0') as u16) * 64
                    + ((octal[1] - b'0') as u16) * 8
                    + (octal[2] - b'0') as u16;
                if value == 0 || value > 255 {
                    return Err(failure());
                }
                decoded.push(value as u8);
                index += 4;
            } else {
                if encoded[index] == 0 {
                    return Err(failure());
                }
                decoded.push(encoded[index]);
                index += 1;
            }
        }
        let mount = PathBuf::from(OsString::from_vec(decoded));
        if !mount.is_absolute() {
            return Err(failure());
        }
        if !path.starts_with(&mount) {
            continue;
        }
        let depth = mount.components().count();
        let local_disk = matches!(
            fields[separator + 1],
            b"ext2" | b"ext3" | b"ext4" | b"xfs" | b"btrfs" | b"f2fs" | b"zfs"
        );
        match best_depth {
            None => {
                best_depth = Some(depth);
                supported = local_disk;
            }
            Some(best) if depth > best => {
                best_depth = Some(depth);
                supported = local_disk;
            }
            Some(best) if depth == best => {
                supported &= local_disk;
            }
            _ => {}
        }
    }
    if best_depth.is_none() || !supported {
        return Err(failure());
    }
    Ok(())
}
