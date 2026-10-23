//! Bounded, no-follow removal of an explicitly reviewed, stopped installation.
//! This is same-account filesystem hygiene, not confinement from a hostile OS user.
use crate::local_files::{self, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_ENTRIES: usize = 200_000;
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Link,
    #[cfg(windows)]
    DirectoryLink,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    size: u64,
    modified: Option<SystemTime>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    root: PathBuf,
    entries: BTreeMap<PathBuf, Entry>,
    pub bytes: u64,
}

fn metadata(path: &Path) -> Result<Entry> {
    let m = fs::symlink_metadata(path).map_err(|_| "uninstall_path_unavailable")?;
    let link = m.file_type().is_symlink();
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        link || m.file_attributes() & 0x400 != 0
    };
    let kind = if link {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x10 != 0 {
                Kind::DirectoryLink
            } else {
                Kind::Link
            }
        }
        #[cfg(not(windows))]
        {
            Kind::Link
        }
    } else if m.is_dir() {
        Kind::Directory
    } else if m.is_file() {
        Kind::File
    } else {
        return Err("uninstall_special_file_refused");
    };
    Ok(Entry {
        kind,
        size: m.len(),
        modified: m.modified().ok(),
    })
}
impl Tree {
    pub fn scan(root: &Path) -> Result<Self> {
        local_files::guard(root)?;
        if root.parent().is_none_or(|p| p.parent().is_none()) {
            return Err("uninstall_dedicated_root_required");
        }
        if metadata(root)?.kind != Kind::Directory {
            return Err("uninstall_directory_required");
        }
        let mut result = Self {
            root: root.to_owned(),
            entries: BTreeMap::new(),
            bytes: 0,
        };
        let mut pending = vec![(root.to_owned(), 0)];
        let mut path_bytes = 0usize;
        while let Some((path, depth)) = pending.pop() {
            let bytes = path.as_os_str().as_encoded_bytes().len();
            path_bytes = path_bytes.saturating_add(bytes);
            if bytes > 4096 || path_bytes > 64 * 1024 * 1024 {
                return Err("uninstall_tree_limit_reached");
            }
            if result.entries.len() >= MAX_ENTRIES || depth > 128 {
                return Err("uninstall_tree_limit_reached");
            }
            let entry = metadata(&path)?;
            if entry.kind == Kind::Directory {
                for child in fs::read_dir(&path).map_err(|_| "uninstall_directory_unavailable")? {
                    if pending.len() + result.entries.len() >= MAX_ENTRIES {
                        return Err("uninstall_tree_limit_reached");
                    }
                    pending.push((
                        child.map_err(|_| "uninstall_directory_unavailable")?.path(),
                        depth + 1,
                    ));
                }
            } else if entry.kind == Kind::File {
                result.bytes = result.bytes.saturating_add(entry.size);
            }
            result.entries.insert(path, entry);
        }
        Ok(result)
    }
    pub fn remove(self) -> Result<()> {
        // Inventory again after shutdown/confirmation. Never recurse into a path
        // discovered during deletion; new files cause a truthful partial failure.
        if Self::scan(&self.root)? != self {
            return Err("uninstall_files_changed_review_again");
        }
        let mut entries: Vec<_> = self.entries.into_iter().collect();
        entries.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
        for (path, expected) in entries {
            // Guard parent traversal, allowing removal of the link itself.
            local_files::guard(path.parent().ok_or("uninstall_path_refused")?)?;
            let actual = metadata(&path)?;
            // Directory timestamps change as children are removed; their type
            // must still agree. Files/links must retain the scanned observation.
            if actual.kind != expected.kind
                || (expected.kind != Kind::Directory && actual != expected)
            {
                return Err("uninstall_files_changed_review_again");
            }
            match expected.kind {
                Kind::Directory => fs::remove_dir(&path),
                #[cfg(windows)]
                Kind::DirectoryLink => fs::remove_dir(&path),
                Kind::File | Kind::Link => fs::remove_file(&path),
            }
            .map_err(|_| "uninstall_partial_failure_files_remain")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_only_the_scanned_owned_tree_and_refuses_a_changed_plan() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owned");
        fs::create_dir_all(root.join("game/world")).unwrap();
        fs::write(root.join("game/world/data"), b"world").unwrap();
        fs::write(temp.path().join("outside"), b"keep").unwrap();
        let plan = Tree::scan(&root).unwrap();
        fs::write(root.join("new"), b"unreviewed").unwrap();
        assert_eq!(plan.remove(), Err("uninstall_files_changed_review_again"));
        assert!(root.join("game/world/data").is_file());
        Tree::scan(&root).unwrap().remove().unwrap();
        assert_eq!(fs::read(temp.path().join("outside")).unwrap(), b"keep");
        assert!(!root.exists());
    }
    #[cfg(unix)]
    #[test]
    fn internal_symlink_is_unlinked_without_touching_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owned");
        fs::create_dir(&root).unwrap();
        fs::write(temp.path().join("outside"), b"keep").unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), root.join("link")).unwrap();
        Tree::scan(&root).unwrap().remove().unwrap();
        assert_eq!(fs::read(temp.path().join("outside")).unwrap(), b"keep");
    }
    #[cfg(windows)]
    #[test]
    fn internal_junction_is_unlinked_without_traversing_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owned");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"external").unwrap();
        let result = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("junction"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Tree::scan(&root).unwrap().remove().unwrap();
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"external");
    }
}
