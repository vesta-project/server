//! Literal editor executable selection. Never invoke a shell or print config bytes.
use crate::local_files::{self, Result};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Clone, Copy)]
enum Platform {
    Windows,
    Linux,
}
fn executable(path: &Path, platform: Platform) -> bool {
    if !path.is_file() {
        return false;
    }
    match platform {
        Platform::Windows => path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
        Platform::Linux => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
    }
}
fn lookup(name: &OsStr, path: &OsStr, platform: Platform) -> Option<PathBuf> {
    let literal = Path::new(name);
    if literal.is_absolute() {
        return executable(literal, platform).then(|| literal.to_owned());
    }
    // No relative paths, cwd lookup or shell parsing. Windows only direct .exe files.
    if literal.components().count() != 1 || name.is_empty() {
        return None;
    }
    for directory in std::env::split_paths(path).filter(|dir| dir.is_absolute()) {
        let candidate = directory.join(name);
        if executable(&candidate, platform) {
            return Some(candidate);
        }
        if matches!(platform, Platform::Windows) && literal.extension().is_none() {
            let candidate = candidate.with_extension("exe");
            if executable(&candidate, platform) {
                return Some(candidate);
            }
        }
    }
    None
}
fn detect(
    editor: Option<OsString>,
    visual: Option<OsString>,
    path: &OsStr,
    platform: Platform,
) -> Result<PathBuf> {
    // EDITOR first, then VISUAL. Set-but-invalid values fail closed, no fallback.
    if let Some(selected) = editor.or(visual) {
        return lookup(&selected, path, platform)
            .ok_or("local_editor_unavailable_use_literal_executable");
    }
    let defaults: &[&str] = match platform {
        Platform::Windows => &["notepad.exe"],
        Platform::Linux => &["nano", "vi"],
    };
    defaults
        .iter()
        .find_map(|name| lookup(OsStr::new(name), path, platform))
        .ok_or("local_editor_unavailable")
}
pub fn open(config: &Path) -> Result<()> {
    local_files::guard(config)?;
    if !std::fs::symlink_metadata(config).is_ok_and(|m| m.is_file()) {
        return Err("local_config_unavailable");
    }
    let platform = if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        return Err("local_platform_unsupported");
    };
    let editor = detect(
        std::env::var_os("EDITOR"),
        std::env::var_os("VISUAL"),
        &std::env::var_os("PATH").unwrap_or_default(),
        platform,
    )?;
    let status = Command::new(editor)
        .arg(config)
        .status()
        .map_err(|_| "local_editor_failed")?;
    if status.success() {
        Ok(())
    } else {
        Err("local_editor_failed")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn program(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, b"inert-not-launched").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }
    #[test]
    fn injected_editor_and_visual_are_literal_and_take_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let chosen = program(temp.path(), "chosen.exe");
        let other = program(temp.path(), "other.exe");
        let path = std::env::join_paths([temp.path()]).unwrap();
        assert_eq!(
            detect(
                Some(chosen.clone().into_os_string()),
                Some(other.clone().into_os_string()),
                &path,
                Platform::Windows
            )
            .unwrap(),
            chosen
        );
        assert_eq!(
            detect(
                None,
                Some(other.clone().into_os_string()),
                &path,
                Platform::Windows
            )
            .unwrap(),
            other
        );
        assert!(
            detect(
                Some("chosen.exe --secret".into()),
                None,
                &path,
                Platform::Windows
            )
            .is_err()
        );
        assert!(detect(Some("missing.exe".into()), None, &path, Platform::Windows).is_err());
    }
    #[test]
    fn injected_path_fallbacks_nano_then_vi_or_notepad() {
        let temp = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([temp.path()]).unwrap();
        let vi = program(temp.path(), "vi");
        assert_eq!(detect(None, None, &path, Platform::Linux).unwrap(), vi);
        let nano = program(temp.path(), "nano");
        assert_eq!(detect(None, None, &path, Platform::Linux).unwrap(), nano);
        let notepad = program(temp.path(), "notepad.exe");
        assert_eq!(
            detect(None, None, &path, Platform::Windows).unwrap(),
            notepad
        );
    }
}
