//! Pure relative-path and case-insensitive file/directory declaration policy.
use super::model::*;
use std::collections::{BTreeSet, HashMap, HashSet};

pub(super) fn relative(path: &str, plus: impl Fn(usize, &str) -> bool) -> Result<()> {
    if path.is_empty() || path.len() > 512 || path.split('/').count() > 16 {
        return Err(InstallError::PathRefused);
    }
    for (i, part) in path.split('/').enumerate() {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part.len() > 128
            || part.starts_with('.')
            || part.ends_with('.')
            || !part.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'-')
                    || (b == b'+' && plus(i, part))
            })
            || matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONTROL" | "STATE"
            )
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(InstallError::PathRefused);
        }
    }
    Ok(())
}
/// The final two components of a library destination are its validated Maven
/// version directory and version-bearing filename; callers also check Maven roundtrip.
pub(super) fn destination(path: &str) -> Result<()> {
    if !path.starts_with("software/") {
        return Err(InstallError::PathRefused);
    }
    let n = path.split('/').count();
    relative(path, |i, _| {
        path.starts_with("software/libraries/") && n >= 6 && i + 2 >= n
    })
}
pub(super) fn declarations<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    cap: usize,
) -> Result<BTreeSet<String>> {
    let mut files = HashSet::new();
    let mut spellings = HashMap::new();
    let mut directories = BTreeSet::new();
    for path in paths {
        if !files.insert(path.to_ascii_lowercase()) {
            return Err(InstallError::PathRefused);
        }
        let parts: Vec<_> = path.split('/').collect();
        let mut prefix = String::new();
        for (i, part) in parts.iter().enumerate() {
            if i != 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(old) = spellings.insert(prefix.to_ascii_lowercase(), prefix.clone()) {
                if old != prefix {
                    return Err(InstallError::PathRefused);
                }
            }
            if i + 1 < parts.len() {
                directories.insert(prefix.clone());
            }
        }
        if files
            .len()
            .checked_add(directories.len())
            .is_none_or(|n| n > cap)
        {
            return Err(InstallError::PathRefused);
        }
    }
    if directories
        .iter()
        .any(|p| files.contains(&p.to_ascii_lowercase()))
    {
        return Err(InstallError::PathRefused);
    }
    Ok(directories)
}
