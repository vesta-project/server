//! Lexical path presentation and comparison helpers for setup.
//!
//! These functions never resolve paths through the filesystem. Call the setup
//! traversal/link guard before normalizing user-provided paths.

use std::path::{Path, PathBuf};

/// Return a path using the host platform's native separators.
///
/// On Unix, backslash is an ordinary filename character and is preserved.
/// On Windows, both slash spellings are accepted and returned as backslashes.
pub fn normalize(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let native: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if unit == b'/' as u16 {
                    b'\\' as u16
                } else {
                    unit
                }
            })
            .collect();
        PathBuf::from(OsString::from_wide(&native))
            .components()
            .collect()
    }
    #[cfg(not(windows))]
    {
        path.components().collect()
    }
}

/// Format a path for people, removing Windows verbatim prefixes when present.
/// UNC verbatim paths such as `\\?\UNC\host\share` display as `\\host\share`.
pub fn display(path: &Path) -> String {
    #[cfg(windows)]
    {
        let raw = normalize(path).to_string_lossy().into_owned();
        strip_windows_verbatim(&raw)
    }
    #[cfg(not(windows))]
    {
        normalize(path).to_string_lossy().into_owned()
    }
}

/// Produce a lexical comparison key for root identity and overlap checks.
///
/// Separators are made consistent and trailing separators are removed except
/// for a drive root. Component-boundary checks can then compare equality and
/// `key + "/"` prefixes without confusing `host` with `host-backup`.
pub fn comparable(path: &Path) -> String {
    let shown = display(path);
    #[cfg(windows)]
    {
        let mut key = shown.replace('\\', "/").to_lowercase();
        while key.ends_with('/') && !is_windows_drive_root(&key) {
            key.pop();
        }
        key
    }
    #[cfg(not(windows))]
    {
        let trimmed = shown.trim_end_matches('/');
        if trimmed.is_empty() && shown.starts_with('/') {
            "/".to_owned()
        } else {
            trimmed.to_owned()
        }
    }
}

#[cfg(windows)]
fn strip_windows_verbatim(path: &str) -> String {
    const VERBATIM_UNC: &str = "\\\\?\\UNC\\";
    const VERBATIM: &str = "\\\\?\\";
    if path
        .get(..VERBATIM_UNC.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(VERBATIM_UNC))
    {
        format!("\\\\{}", &path[VERBATIM_UNC.len()..])
    } else if path
        .get(..VERBATIM.len())
        .is_some_and(|prefix| prefix == VERBATIM)
    {
        path[VERBATIM.len()..].to_owned()
    } else {
        path.to_owned()
    }
}

#[cfg(windows)]
fn is_windows_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}
