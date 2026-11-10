//! Trusted local configuration reads, not browser scope or remote authorization.
use super::{FileError, Result, io_error, native, root};
use std::{fs, io::Read, path::Path};

pub(crate) const MAX_LOCAL_DOCUMENT_BYTES: usize = 1_048_576;

/// Trusted composition only. Creates one directory under observed plain ancestors;
/// no recursive traversal through caller-selected links or replacement of entries.
pub(crate) fn create_local_directory(path: &Path, allow_existing: bool) -> Result<()> {
    root::absolute(path)?;
    root::plain_ancestors(path.parent().ok_or(FileError::InvalidRoot)?)?;
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            root::plain(&meta)?;
            if allow_existing && meta.is_dir() {
                Ok(())
            } else {
                Err(FileError::InvalidRoot)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(io_error)?;
            root::plain_ancestors(path)
        }
        Err(e) => Err(io_error(e)),
    }
}

pub(crate) fn read_local_document(path: &Path, limit: usize) -> Result<Vec<u8>> {
    if limit == 0 || limit > MAX_LOCAL_DOCUMENT_BYTES {
        return Err(FileError::InvalidLimit);
    }
    root::absolute(path)?;
    let parent = path.parent().ok_or(FileError::InvalidRoot)?;
    root::plain_ancestors(parent)?;
    root::plain(&fs::symlink_metadata(path).map_err(io_error)?)?;
    let file = native::open_regular(path)?;
    let actual = native::actual_path(&file, path)?;
    // Match the actual held leaf to the requested canonical path. Ancestor
    // observations remain check-then-use, not same-account race-free isolation.
    if fs::canonicalize(path).map_err(io_error)? != actual {
        return Err(FileError::LinkedPath);
    }
    if file.metadata().map_err(io_error)?.len() > limit as u64 {
        return Err(FileError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit {
        return Err(FileError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused<T>(result: Result<T>, code: &str) {
        assert_eq!(result.err().expect("local read must refuse").code(), code);
    }

    #[test]
    fn trusted_documents_include_browser_protected_names_and_binary_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wrapper-config.toml");
        fs::write(&path, b"trusted\0document\xff").unwrap();
        assert_eq!(
            read_local_document(&path, 17).unwrap(),
            b"trusted\0document\xff"
        );
        // Parsing/secret handling belongs to the configuration consumer. This
        // crate-private native helper grants no browser or management authority.
        refused(read_local_document(&path, 16), "file_too_large");
    }

    #[test]
    fn local_document_limits_are_inclusive_and_missing_or_nonregular_refuse() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("document");
        refused(read_local_document(&path, 1), "file_not_found");
        refused(read_local_document(&path, 0), "file_limit_invalid");
        refused(
            read_local_document(&path, MAX_LOCAL_DOCUMENT_BYTES + 1),
            "file_limit_invalid",
        );
        refused(
            read_local_document(Path::new("relative"), 1),
            "file_root_invalid",
        );
        fs::create_dir(&path).unwrap();
        // The native leaf adapter may refuse opening a directory before its
        // regular-file check; either fixed failure exposes no native diagnostic.
        assert!(read_local_document(&path, 1).is_err());
        fs::remove_dir(&path).unwrap();
        let expected = vec![42; MAX_LOCAL_DOCUMENT_BYTES];
        fs::write(&path, &expected).unwrap();
        assert_eq!(
            read_local_document(&path, MAX_LOCAL_DOCUMENT_BYTES).unwrap(),
            expected
        );
        fs::write(&path, vec![42; MAX_LOCAL_DOCUMENT_BYTES + 1]).unwrap();
        refused(
            read_local_document(&path, MAX_LOCAL_DOCUMENT_BYTES),
            "file_too_large",
        );
        fs::write(&path, []).unwrap();
        assert!(read_local_document(&path, 1).unwrap().is_empty());
        fs::write(&path, vec![42; MAX_LOCAL_DOCUMENT_BYTES + 1]).unwrap();
        refused(
            read_local_document(&path, MAX_LOCAL_DOCUMENT_BYTES + 1),
            "file_limit_invalid",
        );
    }

    #[test]
    fn hard_linked_local_documents_refuse_without_changing_either_name() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        let alias = temp.path().join("alias");
        fs::write(&original, b"protected").unwrap();
        fs::hard_link(&original, &alias).unwrap();
        refused(read_local_document(&alias, 100), "file_path_linked");
        assert_eq!(fs::read(&original).unwrap(), b"protected");
        assert_eq!(fs::read(&alias).unwrap(), b"protected");
    }

    #[cfg(windows)]
    #[test]
    fn local_document_junction_ancestor_refuses_without_touching_target() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("document"), b"protected").unwrap();
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&alias)
            .arg(&real)
            .status()
            .unwrap();
        assert!(status.success(), "create owned local-document junction");
        refused(
            read_local_document(&alias.join("document"), 100),
            "file_path_linked",
        );
        fs::remove_dir(&alias).unwrap();
        assert_eq!(fs::read(real.join("document")).unwrap(), b"protected");
    }

    #[cfg(unix)]
    #[test]
    fn local_document_symlinks_refuse_at_leaf_and_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("document"), b"protected").unwrap();
        std::os::unix::fs::symlink(&real, temp.path().join("alias")).unwrap();
        std::os::unix::fs::symlink(real.join("document"), temp.path().join("leaf")).unwrap();
        refused(
            read_local_document(&temp.path().join("alias/document"), 100),
            "file_path_linked",
        );
        refused(
            read_local_document(&temp.path().join("leaf"), 100),
            "file_path_linked",
        );
        assert_eq!(fs::read(real.join("document")).unwrap(), b"protected");
    }
}
