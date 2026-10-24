//! Bounded, shared browser visibility rules. Default is empty. This affects
//! FileBrowser access only, not pack builds or backup selection.
use super::{FileError, Result, io_error, native};
use std::{io::Read, path::Path};
const MAX_BYTES: u64 = 16 * 1024;
pub(super) fn hidden(root: &Path, path: &str) -> Result<bool> {
    // Ignore configuration remains visible/editable; it cannot override protected
    // names, which Root checks first. A linked/oversized policy fails closed.
    if path == ".vesta-fileignore" {
        return Ok(false);
    }
    let file = match native::open_regular(&root.join(".vesta-fileignore")) {
        Ok(f) => f,
        Err(FileError::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    if file.metadata().map_err(io_error)?.len() > MAX_BYTES {
        return Err(FileError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(FileError::TooLarge);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| FileError::InvalidPath)?;
    let mut hidden = false;
    let mut count = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        count += 1;
        if count > 128 || line.len() > 256 || !line.is_ascii() {
            return Err(FileError::TooLarge);
        }
        let (negate, pattern) = if let Some(p) = line.strip_prefix('!') {
            (true, p)
        } else {
            (false, line)
        };
        let anchored = pattern.starts_with('/');
        let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
        if pattern.is_empty()
            || pattern.contains('\\')
            || pattern.contains(':')
            || pattern.split('/').any(|p| p == ".." || p == ".")
        {
            return Err(FileError::InvalidPath);
        }
        let matches = if anchored || pattern.contains('/') {
            glob(pattern, path)
                || path
                    .match_indices('/')
                    .any(|(i, _)| glob(pattern, &path[..i]))
        } else {
            path.split('/').any(|p| glob(pattern, p))
        };
        if matches {
            hidden = !negate;
        }
    }
    Ok(hidden)
}
// Portable ASCII case-insensitive glob. '*' matches any run (including '/');
// '?' matches one byte. No regex/backtracking, classes, braces or escapes.
fn glob(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == b'?' || p[i].eq_ignore_ascii_case(&t[j])) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_policy_is_bounded_and_protected() {
        let t = tempfile::tempdir().unwrap();
        assert!(!hidden(t.path(), "secret.txt").unwrap());
        std::fs::write(
            t.path().join(".vesta-fileignore"),
            "# browser only\nsecret*\n!secret-public.txt\n/cache/\n*.key\n",
        )
        .unwrap();
        assert!(hidden(t.path(), "secret.txt").unwrap());
        assert!(!hidden(t.path(), "secret-public.txt").unwrap());
        assert!(hidden(t.path(), "cache/nested/file").unwrap());
        assert!(hidden(t.path(), "a/private.KEY").unwrap());
        assert!(!hidden(t.path(), ".vesta-fileignore").unwrap());
    }
}
