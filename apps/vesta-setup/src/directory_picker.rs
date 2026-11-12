//! Read-only, bounded directory previews for OS-local setup.
use crate::{
    engine, paths,
    ui::{Row, View},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

const LIMIT: usize = 1024;
pub(crate) const PAGE_SIZE: usize = 5;

pub(crate) struct Entry {
    pub path: PathBuf,
    pub directory: bool,
}
pub(crate) struct Listing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}
pub(crate) fn list(path: &Path) -> engine::Result<Listing> {
    engine::guard(path)?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in fs::read_dir(path)
        .map_err(|_| "Cannot read this folder. Enter another path.")?
        .take(LIMIT + 1)
    {
        if entries.len() == LIMIT {
            truncated = true;
            break;
        }
        let entry = entry.map_err(|_| "Cannot read this folder. Enter another path.")?;
        let file_type = entry
            .file_type()
            .map_err(|_| "Cannot inspect this folder entry.")?;
        let path = entry.path();
        let directory = file_type.is_dir() && engine::guard(&path).is_ok();
        entries.push(Entry { path, directory });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
    });
    Ok(Listing { entries, truncated })
}
pub(crate) fn validate_server(path: &Path) -> engine::Result<()> {
    engine::guard(path)?;
    if !path.is_dir() {
        return Err("Choose an existing server directory.".into());
    }
    if !["server.properties", "eula.txt", "world", "mods", "plugins"]
        .iter()
        .any(|name| path.join(name).exists())
    {
        return Err("No Minecraft server files were detected in this directory.".into());
    }
    Ok(())
}
pub(crate) fn view(path: &Path, listing: &Listing, page: usize, error: Option<String>) -> View {
    let pages = listing.entries.len().div_ceil(PAGE_SIZE).max(1);
    let mut rows = vec![Row::Detail {
        label: "Current folder".into(),
        value: paths::display(path),
    }];
    if let Some(text) = error {
        rows.push(Row::Notice {
            tone: crate::ui::Tone::Warning,
            text,
        });
    }
    if listing.entries.is_empty() {
        rows.push(Row::Notice {
            tone: crate::ui::Tone::Info,
            text: "This folder is empty.".into(),
        });
    }
    if listing.truncated {
        rows.push(Row::Notice {
            tone: crate::ui::Tone::Warning,
            text: "Showing the first 1024 entries. Enter a path to find another folder.".into(),
        });
    }
    for (label, disabled) in [
        ("Select this folder".to_string(), false),
        ("Go to parent folder".to_string(), path.parent().is_none()),
        ("Enter a different path".to_string(), false),
        (format!("Previous page ({}/{pages})", page + 1), page == 0),
        ("Next page".to_string(), page + 1 >= pages),
    ] {
        rows.push(Row::Choice {
            label,
            description: String::new(),
            selected: false,
            disabled,
        });
    }
    for entry in listing
        .entries
        .iter()
        .skip(page * PAGE_SIZE)
        .take(PAGE_SIZE)
    {
        rows.push(Row::Choice {
            label: format!(
                "{} {}",
                if entry.directory {
                    "[folder]"
                } else {
                    "[file/link]"
                },
                entry.path.file_name().unwrap_or_default().to_string_lossy()
            ),
            description: String::new(),
            selected: false,
            disabled: !entry.directory,
        });
    }
    View {
        step: 2,
        heading: "Connect an existing server".into(),
        description: vec![
            "Browse the folder contents before selecting. Nothing here is modified.".into(),
        ],
        rows,
        hint: "Up/Down to choose · Enter to open/select · Esc to go back".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_orders_folders_shows_files_and_preserves_source() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("world")).unwrap();
        fs::write(temp.path().join("server.properties"), b"witness").unwrap();
        fs::write(temp.path().join("eula.txt"), b"eula=false").unwrap();
        let listing = list(temp.path()).unwrap();
        assert_eq!(listing.entries.len(), 3);
        assert!(listing.entries[0].directory);
        assert!(!listing.entries[1].directory);
        validate_server(temp.path()).unwrap();
        let screen = crate::ui::render(
            &view(temp.path(), &listing, 0, None),
            crate::ui::Theme {
                color: false,
                unicode: false,
                width: 88,
            },
        );
        assert!(screen.contains("world"));
        assert!(screen.contains("server.properties"));
        assert!(screen.contains("eula.txt"));
        assert_eq!(
            fs::read(temp.path().join("eula.txt")).unwrap(),
            b"eula=false"
        );
        assert_eq!(
            fs::read(temp.path().join("server.properties")).unwrap(),
            b"witness"
        );
        assert!(validate_server(&temp.path().join("world")).is_err());
    }
    #[test]
    fn listing_and_pages_are_bounded_and_parent_paths_refused() {
        let temp = tempfile::tempdir().unwrap();
        for i in 0..LIMIT + 2 {
            fs::write(temp.path().join(format!("file-{i}")), b"").unwrap();
        }
        let listing = list(temp.path()).unwrap();
        assert_eq!(listing.entries.len(), LIMIT);
        assert!(listing.truncated);
        assert_eq!(
            view(temp.path(), &listing, 1, None)
                .rows
                .iter()
                .filter(|r| matches!(r, Row::Choice { .. }))
                .count(),
            5 + PAGE_SIZE
        );
        assert!(list(&temp.path().join("..")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn links_cannot_be_browsed_or_selected() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("actual")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("actual"), temp.path().join("linked")).unwrap();
        let listing = list(temp.path()).unwrap();
        assert!(
            !listing
                .entries
                .iter()
                .find(|e| e.path.ends_with("linked"))
                .unwrap()
                .directory
        );
        assert!(list(&temp.path().join("linked")).is_err());
    }
}
