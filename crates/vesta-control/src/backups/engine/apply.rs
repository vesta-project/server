//! In-place restore: make part or all of a live directory match a snapshot.
//!
//! The caller guarantees nothing else writes the directory (game stopped, starts and
//! Vesta file mutations blocked). Ignored and protected paths are never written or
//! removed. Everything is checked before the first change: manifest paths, requested
//! paths, referenced chunks and the target tree (links and special files refuse).
//! Files are replaced one at a time through a verified temporary file and a rename, so
//! each file is either old or new; the whole tree is not atomic, which is why every
//! failure after the first change reports `modified`.
use super::{
    CHUNK_SIZE, Engine, EngineError, FileEntry, MAX_CHUNKS_LISTED, MAX_FILES, hash_hex,
    literal_path, native, valid_hash,
};
use crate::backups::ignore::Ignore;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::Read,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

/// Temporary files are named with this prefix next to their target. Backups skip
/// them; a later restore of their directory removes any left by a crash.
pub(super) const TEMP_PREFIX: &str = ".vesta-restore-";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplySummary {
    pub files_written: u64,
    pub files_unchanged: u64,
    pub files_removed: u64,
    pub dirs_removed: u64,
}

/// A failed apply; `modified` is whether the target changed before the failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplyError {
    pub error: EngineError,
    pub modified: bool,
}
impl From<EngineError> for ApplyError {
    fn from(error: EngineError) -> Self {
        Self {
            error,
            modified: false,
        }
    }
}

/// `path` or one of its ancestors is excluded by the ignore rules.
fn ignored(ignore: &Ignore, path: &str, is_dir: bool) -> bool {
    let mut prefix = String::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        let last = index + 1 == segments.len();
        if ignore.excluded(&prefix, !last || is_dir) {
            return true;
        }
    }
    false
}

/// `path` is or lies under a protected path. A directory merely containing one is
/// not protected itself; it simply cannot be removed while that entry remains.
/// Windows names are case-insensitive, so matching folds case there.
fn protected(paths: &[String], path: &str) -> bool {
    let fold = |s: &str| {
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let path = fold(path);
    paths
        .iter()
        .map(|p| fold(p))
        .any(|p| path == p || path.starts_with(&format!("{p}/")))
}

fn within(scope: &[String], path: &str) -> bool {
    scope.is_empty()
        || scope
            .iter()
            .any(|s| path == s || path.starts_with(&format!("{s}/")))
}

fn plain(path: &Path) -> std::io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn is_plain(meta: &fs::Metadata) -> bool {
    crate::files::plain_entry(meta).is_ok()
}

/// The current tree inside `scope`: plain directories and files that are neither
/// ignored nor protected. Any link or special entry there refuses the restore.
struct Current {
    dirs: BTreeSet<String>,
    files: BTreeMap<String, u64>,
}

fn current(
    root: &Path,
    scope: &[String],
    ignore: &Ignore,
    keep: &[String],
) -> std::result::Result<Current, EngineError> {
    let mut found = Current {
        dirs: BTreeSet::new(),
        files: BTreeMap::new(),
    };
    let mut stack = vec![String::new()];
    while let Some(relative) = stack.pop() {
        let entries = fs::read_dir(native(root, &relative)).map_err(|_| EngineError::Failed)?;
        for entry in entries {
            let entry = entry.map_err(|_| EngineError::Failed)?;
            let name = entry
                .file_name()
                .to_str()
                .ok_or(EngineError::TargetUnsafe)?
                .to_owned();
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let meta = entry
                .path()
                .symlink_metadata()
                .map_err(|_| EngineError::Failed)?;
            let is_dir = meta.is_dir() && is_plain(&meta);
            // Descend through ancestors of the scope even when they are out of scope.
            let leads_into_scope = scope.iter().any(|s| s.starts_with(&format!("{path}/")));
            if ignore.excluded(&path, is_dir) || protected(keep, &path) {
                continue;
            }
            if !within(scope, &path) {
                if leads_into_scope && is_dir {
                    stack.push(path);
                }
                continue;
            }
            if !is_plain(&meta) {
                return Err(EngineError::TargetUnsafe);
            }
            if found.dirs.len() + found.files.len() >= MAX_FILES {
                return Err(EngineError::TooLarge);
            }
            if meta.is_dir() {
                found.dirs.insert(path.clone());
                stack.push(path);
            } else {
                found.files.insert(path, meta.len());
            }
        }
    }
    Ok(found)
}

/// Whether an existing file already holds exactly these chunks.
fn same_content(path: &Path, entry: &FileEntry) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buffer = vec![0u8; CHUNK_SIZE];
    for expected in &entry.chunks {
        let mut filled = 0;
        while filled < CHUNK_SIZE {
            match file.read(&mut buffer[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        if filled == 0 || hash_hex(&buffer[..filled]) != *expected {
            return false;
        }
    }
    matches!(file.read(&mut buffer[..1]), Ok(0))
}

fn set_modified(file: &fs::File, modified_ms: i64) {
    if let Ok(ms) = u64::try_from(modified_ms) {
        let _ = file.set_modified(UNIX_EPOCH + Duration::from_millis(ms));
    }
}

impl Engine {
    /// Make `root` match `snapshot`, either entirely or only at and under `includes`.
    /// In scope, files and directories absent from the snapshot are removed, and
    /// snapshot files are written unless their content already matches. Paths
    /// excluded by `ignore` or covered by `keep` (relative paths, each covering
    /// everything under it) are left exactly as they are. With `reserve`, the restore
    /// is refused unless the target disk keeps that many bytes free afterwards.
    pub fn apply(
        &self,
        snapshot: &str,
        root: &Path,
        includes: &[String],
        ignore: &Ignore,
        keep: &[String],
        reserve: Option<u64>,
    ) -> std::result::Result<ApplySummary, ApplyError> {
        if includes
            .iter()
            .any(|p| !literal_path(p) || crate::files::RelativePath::parse(p).is_err())
        {
            return Err(EngineError::Invalid.into());
        }
        let manifest = self.manifest(snapshot)?;
        // The manifest is untrusted: every path must be valid and unambiguous.
        let mut kinds: BTreeMap<&str, bool> = BTreeMap::new();
        for (path, is_dir) in manifest
            .dirs
            .iter()
            .map(|d| (d.as_str(), true))
            .chain(manifest.files.iter().map(|f| (f.path.as_str(), false)))
        {
            if crate::files::RelativePath::parse(path).is_err()
                || kinds.insert(path, is_dir).is_some()
            {
                return Err(EngineError::Corrupt.into());
            }
        }
        // Every parent of a listed entry is itself a listed directory.
        if kinds.keys().any(|path| {
            path.rsplit_once('/')
                .is_some_and(|(parent, _)| kinds.get(parent) != Some(&true))
        }) {
            return Err(EngineError::Corrupt.into());
        }
        for include in includes {
            if !kinds.contains_key(include.as_str()) {
                return Err(EngineError::NotInSnapshot.into());
            }
            if protected(keep, include) || ignored(ignore, include, kinds[include.as_str()]) {
                return Err(EngineError::TargetUnsafe.into());
            }
        }
        let wanted = |path: &str, is_dir: bool| {
            within(includes, path) && !ignored(ignore, path, is_dir) && !protected(keep, path)
        };
        let dirs: Vec<&String> = manifest.dirs.iter().filter(|d| wanted(d, true)).collect();
        let files: Vec<&FileEntry> = manifest
            .files
            .iter()
            .filter(|f| wanted(&f.path, false))
            .collect();
        // Every chunk must exist before anything changes.
        let stored: HashSet<String> = self
            .store
            .list("chunks/", MAX_CHUNKS_LISTED)
            .map_err(EngineError::from)?
            .into_iter()
            .filter_map(|o| o.key.rsplit('/').next().map(str::to_owned))
            .collect();
        if files
            .iter()
            .flat_map(|f| &f.chunks)
            .any(|h| !valid_hash(h) || !stored.contains(h))
        {
            return Err(EngineError::Corrupt.into());
        }
        // Ancestors of the scope outside it must already be plain directories (or absent).
        for include in includes {
            let ancestors = include
                .match_indices('/')
                .map(|(index, _)| &include[..index]);
            for prefix in ancestors {
                match plain(&native(root, prefix)) {
                    Ok(None) => break,
                    Ok(Some(meta)) if meta.is_dir() && is_plain(&meta) => {}
                    _ => return Err(EngineError::TargetUnsafe.into()),
                }
            }
        }
        let now = current(root, includes, ignore, keep)?;
        // An entry that must become a directory but holds kept content cannot be
        // replaced; refuse before changing anything.
        for path in now.dirs.iter() {
            if kinds.get(path.as_str()) == Some(&false) && wanted(path, false) {
                let mut keeps_content = false;
                let prefix = format!("{path}/");
                for entry in fs::read_dir(native(root, path)).map_err(|_| EngineError::Failed)? {
                    let entry = entry.map_err(|_| EngineError::Failed)?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let child = format!("{prefix}{name}");
                    if !now.dirs.contains(&child) && !now.files.contains_key(&child) {
                        keeps_content = true;
                    }
                }
                if keeps_content {
                    return Err(EngineError::TargetUnsafe.into());
                }
            }
        }

        if let Some(reserve) = reserve {
            // Growth of the restored area, plus one temporary copy of the largest file,
            // which briefly coexists with the file it replaces.
            let incoming: u64 = files.iter().map(|f| f.size).sum();
            let outgoing: u64 = now.files.values().sum();
            let largest = files.iter().map(|f| f.size).max().unwrap_or(0);
            let needed = incoming
                .saturating_sub(outgoing)
                .saturating_add(largest)
                .saturating_add(reserve);
            if crate::backups::space::available(root).is_none_or(|free| free < needed) {
                return Err(EngineError::DiskSpaceLow.into());
            }
        }
        let mut summary = ApplySummary::default();
        let mut modified = false;
        let fail = |error: EngineError, modified: bool| ApplyError { error, modified };

        // 1. Remove files absent from the snapshot (as files), then empty directories
        //    absent from it (as directories), deepest first.
        let snapshot_files: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let snapshot_dirs: HashSet<&str> = dirs.iter().map(|d| d.as_str()).collect();
        for path in now.files.keys() {
            if !snapshot_files.contains(path.as_str()) {
                fs::remove_file(native(root, path))
                    .map_err(|_| fail(EngineError::Failed, modified))?;
                modified = true;
                summary.files_removed += 1;
            }
        }
        for path in now.dirs.iter().rev() {
            if !snapshot_dirs.contains(path.as_str()) {
                // A directory still holding ignored or protected entries stays.
                if fs::remove_dir(native(root, path)).is_ok() {
                    modified = true;
                    summary.dirs_removed += 1;
                } else if kinds.get(path.as_str()) == Some(&false) {
                    return Err(fail(EngineError::TargetUnsafe, modified));
                }
            }
        }

        // 2. Directories, then files through a verified temporary file and a rename.
        for dir in &dirs {
            let path = native(root, dir);
            match plain(&path) {
                Ok(Some(meta)) if meta.is_dir() && is_plain(&meta) => {}
                Ok(None) => {
                    fs::create_dir(&path).map_err(|_| fail(EngineError::Failed, modified))?;
                    modified = true;
                }
                _ => return Err(fail(EngineError::TargetUnsafe, modified)),
            }
        }
        for entry in files {
            let path = native(root, &entry.path);
            if let Some((parent, _)) = entry.path.rsplit_once('/')
                && !matches!(plain(&native(root, parent)), Ok(Some(m)) if m.is_dir() && is_plain(&m))
            {
                return Err(fail(EngineError::TargetUnsafe, modified));
            }
            match plain(&path) {
                Ok(Some(meta)) if meta.is_file() && is_plain(&meta) => {
                    if meta.len() == entry.size && same_content(&path, entry) {
                        if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
                            set_modified(&file, entry.modified_ms);
                        }
                        summary.files_unchanged += 1;
                        continue;
                    }
                }
                Ok(None) => {}
                _ => return Err(fail(EngineError::TargetUnsafe, modified)),
            }
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).map_err(|_| fail(EngineError::Failed, modified))?;
            let temp = path.with_file_name(format!("{TEMP_PREFIX}{}.tmp", hex::encode(random)));
            let written = (|| -> std::result::Result<(), EngineError> {
                let mut out = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)
                    .map_err(|_| EngineError::Failed)?;
                self.write_restored_file(entry, &mut out)?;
                set_modified(&out, entry.modified_ms);
                out.sync_all().map_err(|_| EngineError::Failed)?;
                Ok(())
            })();
            if let Err(error) = written {
                let _ = fs::remove_file(&temp);
                return Err(fail(error, modified));
            }
            if fs::rename(&temp, &path).is_err() {
                let _ = fs::remove_file(&temp);
                return Err(fail(EngineError::Failed, modified));
            }
            modified = true;
            summary.files_written += 1;
        }
        Ok(summary)
    }
}
