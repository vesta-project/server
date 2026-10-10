use super::{FileError, RelativePath, Result, io_error, path::protected};
use crate::identity::Resource;
use std::{
    fs::{self, Metadata},
    path::{Component, Path, PathBuf},
};

#[derive(Clone)]
pub(super) struct Root {
    raw: PathBuf,
    canonical: PathBuf,
    protected: Vec<PathBuf>,
    pub(super) remote_documents: bool,
    properties_exception: bool,
}
pub(super) struct CheckedPath {
    pub(super) path: PathBuf,
    pub(super) resource: Resource,
    pub(super) metadata: Metadata,
}
impl Root {
    pub(super) fn open(root: &Path, protected_paths: &[&Path]) -> Result<Self> {
        absolute(root)?;
        plain_ancestors(root)?;
        if !fs::symlink_metadata(root).map_err(io_error)?.is_dir() {
            return Err(FileError::NotDirectory);
        }
        let canonical = fs::canonicalize(root).map_err(io_error)?;
        let mut protected = Vec::new();
        for path in protected_paths {
            absolute(path)?;
            if within(&canonical, path) {
                return Err(FileError::ProtectedPath);
            }
            protected.push(path.to_path_buf());
            // Resolve existing ancestor aliases even when a configured secret or
            // several final components are missing. Retain its raw spelling too.
            let real = exclusion(path)?;
            if within(&canonical, &real) {
                return Err(FileError::ProtectedPath);
            }
            protected.push(real);
        }
        let result = Self {
            raw: root.to_path_buf(),
            canonical,
            protected,
            remote_documents: false,
            properties_exception: false,
        };
        result.resource(&result.canonical)?;
        Ok(result)
    }
    pub(super) fn remote(mut self, properties_exception: bool) -> Self {
        self.remote_documents = true;
        self.properties_exception = properties_exception;
        self
    }
    pub(super) fn resolve(&self, relative: Option<&RelativePath>) -> Result<CheckedPath> {
        let requested = relative
            .map(|path| self.canonical.join(path.as_str()))
            .unwrap_or_else(|| self.canonical.clone());
        // Known protected spellings refuse before probing their existence; actual
        // canonical and opened-handle resources are still checked afterward.
        self.resource(&requested)?;
        plain_ancestors(&self.raw)?;
        if fs::canonicalize(&self.raw).map_err(io_error)? != self.canonical {
            return Err(FileError::InvalidRoot);
        }
        let mut path = self.canonical.clone();
        if let Some(relative) = relative {
            for part in relative.as_str().split('/') {
                if !fs::symlink_metadata(&path).map_err(io_error)?.is_dir() {
                    return Err(FileError::NotDirectory);
                }
                let child = path.join(part);
                // Earlier components may have resolved a native short-name alias.
                // Check the now-canonical parent plus child before probing the leaf.
                self.resource(&child)?;
                plain(&fs::symlink_metadata(&child).map_err(io_error)?)?;
                let actual = fs::canonicalize(&child).map_err(io_error)?;
                if actual.parent() != Some(path.as_path()) {
                    return Err(FileError::LinkedPath);
                }
                self.resource(&actual)?;
                path = actual;
            }
        }
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        plain(&metadata)?;
        let resource = self.resource(&path)?;
        Ok(CheckedPath {
            path,
            resource,
            metadata,
        })
    }
    pub(super) fn resource(&self, path: &Path) -> Result<Resource> {
        if !within(path, &self.canonical) {
            return Err(FileError::LinkedPath);
        }
        if self
            .protected
            .iter()
            .any(|protected| within(path, protected))
        {
            return Err(FileError::ProtectedPath);
        }
        let relative = path
            .strip_prefix(&self.canonical)
            .map_err(|_| FileError::LinkedPath)?;
        let mut parts = Vec::new();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(FileError::InvalidPath);
            };
            let text = name.to_str().ok_or(FileError::InvalidPath)?;
            if protected(text) {
                return Err(FileError::ProtectedPath);
            }
            parts.push(text);
        }
        let text = parts.join("/");
        if self.remote_documents {
            remote_document(&text, self.properties_exception)?;
        }
        if text.is_empty() {
            Ok(Resource::ROOT)
        } else {
            RelativePath::parse(&text).map(|path| path.resource())
        }
    }
}
/// Observe directory identity with the same native alias and plain-ancestor checks
/// used for file roots. This creates nothing and does not pin future path edits.
pub(crate) fn same_directory(left: &Path, right: &Path) -> Result<bool> {
    absolute(left)?;
    absolute(right)?;
    plain_ancestors(left)?;
    plain_ancestors(right)?;
    let left = fs::canonicalize(left).map_err(io_error)?;
    let right = fs::canonicalize(right).map_err(io_error)?;
    Ok(within(&left, &right) && within(&right, &left))
}

fn exclusion(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    for component in path.components() {
        if let Component::Normal(name) = component {
            let text = name.to_str().ok_or(FileError::InvalidRoot)?;
            if text.ends_with(['.', ' ']) || text.contains(':') {
                return Err(FileError::InvalidRoot);
            }
        }
    }
    let mut ancestor = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut real) => {
                if !suffix.is_empty() && !fs::symlink_metadata(&real).map_err(io_error)?.is_dir() {
                    return Err(FileError::NotDirectory);
                }
                for part in suffix.into_iter().rev() {
                    real.push(part);
                }
                return Ok(real);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A present but unresolvable link is not a missing suffix: do not
                // silently turn it into an unprotected future native alias.
                match fs::symlink_metadata(&ancestor) {
                    Ok(_) => return Err(FileError::LinkedPath),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(FileError::Unavailable),
                }
                let part = ancestor.file_name().ok_or(FileError::InvalidRoot)?;
                suffix.push(part.to_os_string());
                if !ancestor.pop() {
                    return Err(FileError::InvalidRoot);
                }
            }
            Err(_) => return Err(FileError::Unavailable),
        }
    }
}
pub(super) fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.as_os_str().len() > 32_768 {
        return Err(FileError::InvalidRoot);
    }
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(FileError::InvalidRoot);
        }
        #[cfg(windows)]
        if let Component::Prefix(prefix) = component
            && !matches!(
                prefix.kind(),
                std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
            )
        {
            return Err(FileError::InvalidRoot);
        }
    }
    Ok(())
}
pub(super) fn plain_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(io_error)?;
        plain(&metadata)?;
        if !metadata.is_dir() {
            return Err(FileError::NotDirectory);
        }
    }
    Ok(())
}
pub(super) fn plain(metadata: &Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(FileError::LinkedPath);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(FileError::LinkedPath);
        }
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(FileError::NotRegular);
    }
    Ok(())
}
fn within(path: &Path, parent: &Path) -> bool {
    let mut path = path.components();
    for expected in parent.components() {
        let Some(actual) = path.next() else {
            return false;
        };
        #[cfg(windows)]
        let same = match (actual, expected) {
            (Component::Prefix(a), Component::Prefix(b)) => {
                use std::path::Prefix;
                let drive = |prefix| match prefix {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        Some(drive.to_ascii_uppercase())
                    }
                    _ => None,
                };
                drive(a.kind()).is_some() && drive(a.kind()) == drive(b.kind())
            }
            _ => actual
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy()),
        };
        #[cfg(not(windows))]
        let same = actual == expected;
        if !same {
            return false;
        }
    }
    true
}

/// Applies to both submitted and native/opened-handle canonical resources.
pub(super) fn remote_document(text: &str, properties_exception: bool) -> Result<()> {
    if properties_exception && text.eq_ignore_ascii_case("server.properties") {
        return Ok(());
    }
    if text.split('/').any(|part| {
        [
            "server.properties",
            "whitelist.json",
            "banned-players.json",
            "banned-ips.json",
            "ops.json",
        ]
        .iter()
        .any(|name| part.eq_ignore_ascii_case(name))
    }) {
        return Err(FileError::ProtectedPath);
    }
    Ok(())
}
#[cfg(test)]
mod remote_document_tests {
    use super::*;
    #[test]
    fn canonical_documents_are_excluded_and_exception_is_root_properties_only() {
        for name in [
            "server.properties",
            "SERVER.PROPERTIES",
            "whitelist.json",
            "OPS.JSON",
            "banned-players.json",
            "banned-ips.json",
            "config/server.properties",
        ] {
            assert_eq!(remote_document(name, false), Err(FileError::ProtectedPath));
        }
        assert!(remote_document("server.properties", true).is_ok());
        for name in [
            "ops.json",
            "config/server.properties",
            "server.properties/child",
        ] {
            assert_eq!(remote_document(name, true), Err(FileError::ProtectedPath));
        }
    }
}
