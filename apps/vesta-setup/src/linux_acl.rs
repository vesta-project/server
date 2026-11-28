//! Root ACL adapter. Review pins every object; apply addresses only those descriptors.
#![allow(unsafe_code)]
use std::{
    fs::{self, File},
    io::{ErrorKind, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    let roots = Path::new(roots);
    let parent = roots.parent().ok_or("Missing registry directory")?;
    let registry_dir = File::open(parent).map_err(|e| e.to_string())?;
    fs2::FileExt::lock_exclusive(&registry_dir).map_err(|e| e.to_string())?;
    let mut remaining: Vec<String> = match fs::read(roots) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    if action == "remove" && !remaining.iter().any(|r| r == path) {
        return Err("That path is not a registered import folder".into());
    }
    let entries = review(Path::new(path))?;
    let mut changes = Vec::new();
    for e in &entries {
        if e.path == Path::new("/")
            || (action == "remove"
                && e.parent
                && remaining
                    .iter()
                    .any(|r| r != path && Path::new(r).starts_with(&e.path)))
        {
            continue;
        }
        let rule = if action == "remove" {
            "u:vesta".into()
        } else {
            let access = if e.parent {
                "--x"
            } else if e.file.metadata().map_err(|e| e.to_string())?.is_dir() {
                "r-x"
            } else {
                "r--"
            };
            format!("u:vesta:{access},m::r-x")
        };
        changes.push((e, rule));
    }
    apply_acl(
        &entries,
        &changes,
        |fdpath, rule| {
            let status = Command::new("setfacl")
                .args([
                    "--no-mask",
                    if action == "remove" { "-x" } else { "-m" },
                    rule,
                    "--",
                    fdpath,
                ])
                .status()
                .map_err(|e| e.to_string())?;
            status
                .success()
                .then_some(())
                .ok_or_else(|| "Descriptor ACL operation failed".into())
        },
        || {
            if action == "remove" {
                remaining.retain(|r| r != path);
            } else if !remaining.iter().any(|r| r == path) {
                remaining.push(path.clone());
            }
            write_registry(roots, &remaining)
                .map_err(|e| format!("Could not update import folder registry: {e}"))
        },
    )
}

fn write_registry(path: &Path, roots: &[String]) -> std::io::Result<()> {
    let stage = path.with_file_name(format!(".import-roots-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&stage)?;
    let result = (|| {
        serde_json::to_writer(&mut file, roots)?;
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
        file.sync_all()?;
        fs::rename(&stage, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}

fn apply_acl(
    entries: &[Entry],
    changes: &[(&Entry, String)],
    mut change: impl FnMut(&str, &str) -> Result<(), String>,
    commit: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    unchanged(entries)?;
    let mut undo = Vec::new();
    let result = (|| {
        for (entry, rule) in changes {
            unchanged(entries)?;
            let fdpath = format!("/proc/{}/fd/{}", std::process::id(), entry.file.as_raw_fd());
            let snapshot = Command::new("getfacl")
                .args(["--access", "--omit-header", "--numeric", "--", &fdpath])
                .output()
                .map_err(|e| e.to_string())?;
            if !snapshot.status.success() {
                return Err(format!(
                    "Could not read ACL for {}: {}",
                    entry.path.display(),
                    String::from_utf8_lossy(&snapshot.stderr).trim()
                ));
            }
            undo.push((entry, fdpath.clone(), snapshot.stdout));
            change(&fdpath, rule)?;
        }
        unchanged(entries)?;
        commit()
    })();
    if let Err(mut error) = result {
        for (entry, fdpath, acl) in undo.into_iter().rev() {
            if let Err(rollback) = restore_acl(&fdpath, &acl) {
                error.push_str(&format!(
                    "; ACL rollback failed for {}: {rollback}",
                    entry.path.display()
                ));
            }
        }
        return Err(error);
    }
    Ok(())
}

fn restore_acl(fdpath: &str, acl: &[u8]) -> Result<(), String> {
    let mut child = Command::new("setfacl")
        .args(["--no-mask", "--set-file=-", "--", fdpath])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let write = child.stdin.take().unwrap().write_all(acl);
    let status = child.wait().map_err(|e| e.to_string())?;
    write.map_err(|e| e.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "setfacl failed".into())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_acl_restores_existing_entries_on_apply_or_registry_failure() {
        if Command::new("getfacl").arg("--version").output().is_err()
            || Command::new("setfacl").arg("--version").output().is_err()
        {
            eprintln!("ACL regression requires getfacl and setfacl");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let entries: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|name| {
                let path = temp.path().join(name);
                fs::create_dir(&path).unwrap();
                let file = File::open(&path).unwrap();
                Entry {
                    file,
                    path,
                    parent: false,
                }
            })
            .collect();
        assert!(
            Command::new("setfacl")
                .args(["-m", "u:65534:--x,d:u:65534:r-x"])
                .arg(&entries[0].path)
                .status()
                .unwrap()
                .success()
        );
        let get_acl = |entry: &Entry| {
            let output = Command::new("getfacl")
                .args(["-c", "-n", "--"])
                .arg(&entry.path)
                .output()
                .unwrap();
            assert!(output.status.success());
            output.stdout
        };
        let originals: Vec<_> = entries.iter().map(get_acl).collect();
        let changes: Vec<_> = entries
            .iter()
            .map(|entry| (entry, "u:65534:r-x,m::r-x".into()))
            .collect();
        for fail_apply in [true, false] {
            let mut applied = 0;
            let err = apply_acl(
                &entries,
                &changes,
                |fd, rule| {
                    assert!(
                        Command::new("setfacl")
                            .args(["-n", "-m", rule, "--", fd])
                            .status()
                            .unwrap()
                            .success()
                    );
                    applied += 1;
                    if fail_apply && applied == 2 {
                        return Err("apply failed".into());
                    }
                    Ok(())
                },
                || Err("registry failed".into()),
            )
            .unwrap_err();
            assert_eq!(
                err,
                if fail_apply {
                    "apply failed"
                } else {
                    "registry failed"
                }
            );
            assert_eq!(entries.iter().map(get_acl).collect::<Vec<_>>(), originals);
        }
    }
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
