//! Private reuse of the native file boundary for explicit trusted pack preparation.
use super::{FileError, RelativePath, Result, io_error, native, root};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

pub(crate) struct PackRoot(root::Root);
pub(crate) enum PackNode {
    Directory(RelativePath),
    File(RelativePath),
}
impl PackRoot {
    pub(crate) fn open(path: &Path, protected: &[&Path]) -> Result<Self> {
        root::Root::open(path, protected).map(Self)
    }

    /// Only genuinely absent paths are skipped. Protected or hidden selections
    /// are excluded; links, native aliases, regular files and other failures refuse.
    pub(crate) fn selected_directory(&self, path: &RelativePath) -> Result<bool> {
        if path.as_str().split('/').any(|part| part.starts_with('.')) {
            return Err(FileError::ProtectedPath);
        }
        self.0.resolve(None)?;
        let checked = match self.0.resolve(Some(path)) {
            Err(FileError::NotFound) => return Ok(false),
            result => result?,
        };
        if checked.resource.as_str() != path.as_str() {
            return Err(FileError::LinkedPath);
        }
        if !checked.metadata.is_dir() {
            return Err(FileError::NotDirectory);
        }
        Ok(true)
    }

    /// Includes hidden/excluded entries in the caller's global scan-work budget.
    pub(crate) fn children(
        &self,
        directory: Option<&RelativePath>,
        remaining: &mut usize,
    ) -> Result<Vec<PackNode>> {
        let checked = self.0.resolve(directory)?;
        if !checked.metadata.is_dir() {
            return Err(FileError::NotDirectory);
        }
        let mut children = Vec::new();
        for entry in fs::read_dir(&checked.path).map_err(io_error)? {
            if *remaining == 0 {
                return Err(FileError::TooLarge);
            }
            *remaining -= 1;
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(FileError::InvalidPath)?;
            if name.starts_with('.') {
                continue;
            }
            let text = if checked.resource.is_root() {
                name.to_owned()
            } else {
                format!("{}/{name}", checked.resource.as_str())
            };
            let relative = match RelativePath::parse(&text) {
                Ok(path) => path,
                Err(FileError::ProtectedPath) => continue,
                Err(error) => return Err(error),
            };
            let child = match self.0.resolve(Some(&relative)) {
                Ok(child) => child,
                Err(FileError::ProtectedPath) => continue,
                Err(error) => return Err(error),
            };
            // Do not silently rename a native alias into a second catalogue entry.
            if child.resource.as_str() != relative.as_str() {
                return Err(FileError::LinkedPath);
            }
            children.push(if child.metadata.is_dir() {
                PackNode::Directory(relative)
            } else {
                PackNode::File(relative)
            });
        }
        children.sort_by(|a, b| node_path(a).cmp(node_path(b)));
        Ok(children)
    }

    pub(crate) fn pin(&self, relative: &RelativePath, limit: usize) -> Result<(Vec<u8>, String)> {
        let checked = self.0.resolve(Some(relative))?;
        if !checked.metadata.is_file() {
            return Err(FileError::NotRegular);
        }
        let mut file = native::open_regular(&checked.path)?;
        let actual = native::actual_path(&file, &checked.path)?;
        if self.0.resource(&actual)?.as_str() != relative.as_str() {
            return Err(FileError::LinkedPath);
        }
        let metadata = file.metadata().map_err(io_error)?;
        if metadata.len() > limit as u64 {
            return Err(FileError::TooLarge);
        }
        let (bytes, digest) = pin_bytes(&mut file, metadata.len() as usize, limit)?;
        let after = file.metadata().map_err(io_error)?;
        if metadata.len() != bytes.len() as u64
            || after.len() != metadata.len()
            || after.modified().map_err(io_error)? != metadata.modified().map_err(io_error)?
        {
            return Err(FileError::Unavailable);
        }
        // The owned bytes, not the mutable filename, are the artifact identity.
        Ok((bytes, digest))
    }
}

fn pin_bytes(reader: &mut impl Read, expected: usize, limit: usize) -> Result<(Vec<u8>, String)> {
    if expected > limit {
        return Err(FileError::TooLarge);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| FileError::Unavailable)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        // One extra byte detects growth, including a zero remaining total budget.
        let available = (expected - bytes.len()).saturating_add(1).min(buffer.len());
        let read = match reader.read(&mut buffer[..available]) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result.map_err(io_error)?,
        };
        if read == 0 {
            break;
        }
        if read > limit - bytes.len() {
            return Err(FileError::TooLarge);
        }
        if read > expected - bytes.len() {
            return Err(FileError::Unavailable);
        }
        hash.update(&buffer[..read]);
        bytes.extend_from_slice(&buffer[..read]);
    }
    if bytes.len() != expected {
        return Err(FileError::Unavailable);
    }
    Ok((bytes, hex::encode(hash.finalize())))
}

#[cfg(test)]
#[path = "pack_root_tests.rs"]
mod tests;

fn node_path(node: &PackNode) -> &RelativePath {
    match node {
        PackNode::Directory(path) | PackNode::File(path) => path,
    }
}
