//! Root ACL adapter. Review pins every object; apply addresses only those descriptors.
#![allow(unsafe_code)]
use std::{
    fs::{self, File},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};
struct Entry {
    file: File,
    path: PathBuf,
    parent: bool,
}
fn open(parent: &File, name: &std::ffi::OsStr) -> Result<File, String> {
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: owned descriptor, stable single component. Never follow any link.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn review(path: &Path) -> Result<Vec<Entry>, String> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err("Choose an absolute directory other than /".into());
    }
    let mut chain = vec![Entry {
        file: File::open("/").map_err(|e| e.to_string())?,
        path: PathBuf::from("/"),
        parent: true,
    }];
    for part in path.components() {
        if matches!(part, std::path::Component::RootDir) {
            continue;
        }
        let std::path::Component::Normal(name) = part else {
            return Err("Non-normal path".into());
        };
        let parent = chain.last().unwrap();
        let file = open(&parent.file, name)?;
        if !file.metadata().map_err(|e| e.to_string())?.is_dir() {
            return Err("Not a directory".into());
        }
        chain.push(Entry {
            file,
            path: parent.path.join(name),
            parent: true,
        });
    }
    let owner = chain
        .last()
        .unwrap()
        .file
        .metadata()
        .map_err(|e| e.to_string())?
        .uid();
    for e in &chain {
        let m = e.file.metadata().map_err(|e| e.to_string())?;
        // POSIX ACL write permissions are bounded by group mode's ACL mask.
        if (m.uid() != 0 && m.uid() != owner) || m.mode() & 0o022 != 0 {
            return Err(
                "Ancestor is writable by another owner/group; choose a controlled folder".into(),
            );
        }
    }
    chain.last_mut().unwrap().parent = false;
    let mut index = chain.len() - 1;
    while index < chain.len() {
        if chain.len() > 100_000 {
            return Err("Too many ACL objects".into());
        }
        if chain[index]
            .file
            .metadata()
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            let fdpath = format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                chain[index].file.as_raw_fd()
            );
            for child in fs::read_dir(fdpath).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?;
                let file = open(&chain[index].file, &child.file_name())?;
                let m = file.metadata().map_err(|e| e.to_string())?;
                if m.dev() != chain[0].file.metadata().map_err(|e| e.to_string())?.dev()
                    || (m.is_file() && m.nlink() != 1)
                    || (!m.is_dir() && !m.is_file())
                {
                    return Err("Special file refused".into());
                }
                if m.uid() != owner && m.uid() != 0 {
                    return Err("Tree contains another owner's file".into());
                }
                chain.push(Entry {
                    file,
                    path: chain[index].path.join(child.file_name()),
                    parent: false,
                });
            }
        }
        index += 1;
    }
    Ok(chain)
}
fn unchanged(entries: &[Entry]) -> Result<(), String> {
    for e in entries {
        let current = fs::symlink_metadata(&e.path).map_err(|e| e.to_string())?;
        let pinned = e.file.metadata().map_err(|e| e.to_string())?;
        if current.file_type().is_symlink()
            || current.dev() != pinned.dev()
            || current.ino() != pinned.ino()
        {
            return Err("Path changed between review and apply".into());
        }
    }
    Ok(())
}
pub fn run(args: &[String]) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("ACL changes require root".into());
    }
    let [action, path, roots] = args else {
        return Err("Expected ACL action, path and roots file".into());
    };
    if action != "add" && action != "remove" {
        return Err("Unknown ACL action".into());
    }
    let entries = review(Path::new(path))?;
    let remaining: Vec<String> = fs::read(roots)
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    unchanged(&entries)?;
    for e in &entries {
        unchanged(&entries)?;
        if e.path == Path::new("/") {
            continue;
        }
        if action == "remove"
            && e.parent
            && remaining
                .iter()
                .any(|r| r != path && Path::new(r).starts_with(&e.path))
        {
            continue;
        }
        let rule = if action == "remove" {
            "u:vesta"
        } else if e.parent {
            "u:vesta:--x"
        } else if e.file.metadata().map_err(|e| e.to_string())?.is_dir() {
            "u:vesta:r-x"
        } else {
            "u:vesta:r--"
        };
        let fdpath = format!("/proc/{}/fd/{}", std::process::id(), e.file.as_raw_fd());
        let mask_rule = if action == "add" {
            format!("{rule},m::r-x")
        } else {
            rule.into()
        };
        let status = std::process::Command::new("setfacl")
            .args([
                "--no-mask",
                if action == "remove" { "-x" } else { "-m" },
                &mask_rule,
                "--",
                &fdpath,
            ])
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Descriptor ACL operation failed".into());
        }
    }
    unchanged(&entries)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_reviewed_ancestor_swap_refused() {
        let home = std::env::var_os("HOME").unwrap();
        let t = tempfile::tempdir_in(home).unwrap();
        let a = t.path().join("a");
        fs::create_dir_all(a.join("server")).unwrap();
        let pinned = review(&a.join("server")).unwrap();
        fs::rename(&a, t.path().join("old")).unwrap();
        std::os::unix::fs::symlink("/etc", &a).unwrap();
        assert!(unchanged(&pinned).is_err());
    }
    #[test]
    fn linux_ancestor_swap_refused() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        fs::create_dir_all(a.join("server")).unwrap();
        // /tmp is intentionally refused because an unrelated writer can swap ancestors.
        assert!(review(&a.join("server")).is_err());
        let f = File::open(t.path()).unwrap();
        std::os::unix::fs::symlink("/etc", t.path().join("link")).unwrap();
        assert!(open(&f, std::ffi::OsStr::new("link")).is_err());
    }
}
