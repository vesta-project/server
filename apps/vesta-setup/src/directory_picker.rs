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
    pub readable: bool,
    pub entries: Vec<Entry>,
    pub truncated: bool,
    pub skipped: usize,
}
pub(crate) fn list(path: &Path) -> engine::Result<Listing> {
    engine::guard(path).map_err(|code| if code=="setup_path_unavailable" {
        "Cannot inspect this folder. Run setup as its owning OS account or use an accessible copy.".into()
    } else {code})?;
    let entries = fs::read_dir(path)
        .map_err(|error| if error.kind()==std::io::ErrorKind::PermissionDenied {
            "Permission denied. Run setup as the OS account that owns this server, or use an accessible copy."
        } else {"Cannot read this folder. Go to its parent or enter another path."})?
        .map(|entry| {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            let directory = file_type.is_dir() && engine::guard(&path).is_ok();
            Ok(Entry { path, directory })
        });
    Ok(collect_entries(entries))
}
fn collect_entries(entries: impl Iterator<Item = std::io::Result<Entry>>) -> Listing {
    let source = entries;
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut skipped = 0;
    for (examined, entry) in source.take(LIMIT + 1).enumerate() {
        if examined == LIMIT {
            truncated = true;
            break;
        }
        match entry {
            Ok(entry) => entries.push(entry),
            Err(_) => skipped += 1,
        }
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
    });
    Listing {
        readable: true,
        entries,
        truncated,
        skipped,
    }
}
fn visible_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                format!("\\u{{{:x}}}", u32::from(c))
            } else {
                c.escape_debug().collect()
            }
        })
        .collect()
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
    if listing.skipped > 0 {
        rows.push(Row::Notice {
            tone: crate::ui::Tone::Warning,
            text: format!(
                "{} entries could not be inspected and are not shown.",
                listing.skipped
            ),
        });
    }
    if listing.readable && listing.entries.is_empty() && listing.skipped == 0 {
        rows.push(Row::Notice {
            tone: crate::ui::Tone::Info,
            text: "This folder is empty.".into(),
        });
    }
    if listing.truncated {
        rows.push(Row::Notice {
            tone: crate::ui::Tone::Warning,
            text: "Examined the first 1024 entries. Enter a path to find another folder.".into(),
        });
    }
    for (label, disabled) in [
        ("Select this folder".to_string(), !listing.readable),
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
                visible_filename(&entry.path.file_name().unwrap_or_default().to_string_lossy())
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
        hint: "Up/Down to choose · Enter to open/select · Esc to go back. \\u{20} marks a space."
            .into(),
    }
}

pub(crate) fn unavailable() -> Listing {
    Listing {
        readable: false,
        entries: vec![],
        truncated: false,
        skipped: 0,
    }
}

pub(crate) fn preview(path: &Path) -> Vec<Row> {
    match list(path) {
        Ok(listing) => {
            let mut rows = vec![Row::Notice {
                tone: crate::ui::Tone::Success,
                text: "Readable folder. Press Enter to browse it.".into(),
            }];
            rows.extend(listing.entries.iter().take(PAGE_SIZE).map(|entry| {
                Row::Detail {
                    label: if entry.directory {
                        "Folder"
                    } else {
                        "File/link"
                    }
                    .into(),
                    value: visible_filename(
                        &entry.path.file_name().unwrap_or_default().to_string_lossy(),
                    ),
                }
            }));
            rows
        }
        Err(message) => vec![Row::Notice {
            tone: crate::ui::Tone::Warning,
            text: message,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_folder_keeps_parent_and_manual_entry_enabled_and_is_not_empty() {
        let temp = tempfile::tempdir().unwrap();
        let view = view(
            temp.path(),
            &unavailable(),
            0,
            Some("Permission denied".into()),
        );
        let choices: Vec<_> = view
            .rows
            .iter()
            .filter_map(|row| {
                if let Row::Choice { disabled, .. } = row {
                    Some(*disabled)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(&choices[..3], &[true, false, false]);
        let text = crate::ui::render(
            &view,
            crate::ui::Theme {
                color: false,
                unicode: false,
                width: 100,
            },
        );
        assert!(text.contains("Permission denied"));
        assert!(!text.contains("folder is empty"));
        assert!(
            preview(&temp.path().join("missing"))
                .iter()
                .any(|row| matches!(
                    row,
                    Row::Notice {
                        tone: crate::ui::Tone::Warning,
                        ..
                    }
                ))
        );
        fs::write(temp.path().join("server.properties"), b"fixture").unwrap();
        assert!(
            preview(temp.path())
                .iter()
                .any(|row| matches!(row,Row::Detail{value,..} if value=="server.properties"))
        );
    }
    #[test]
    fn child_errors_keep_good_entries_visible_and_bound_all_attempts() {
        use std::{cell::Cell, io};
        let attempts = Cell::new(0);
        let listing = collect_entries((0..LIMIT + 20).map(|index| {
            attempts.set(attempts.get() + 1);
            if index == 1 {
                Ok(Entry {
                    path: PathBuf::from("good-folder"),
                    directory: true,
                })
            } else {
                Err(io::Error::from(io::ErrorKind::NotFound))
            }
        }));
        assert_eq!(listing.entries.len(), 1);
        assert!(listing.entries[0].directory);
        assert!(listing.truncated);
        assert_eq!(listing.skipped, LIMIT - 1);
        assert!(attempts.get() <= LIMIT + 1);
        let screen = crate::ui::render(
            &view(Path::new("/fixture"), &listing, 0, None),
            crate::ui::Theme {
                color: false,
                unicode: false,
                width: 88,
            },
        );
        assert!(screen.contains("good-folder"));
        assert!(screen.contains("could not be inspected"));
    }
    #[test]
    fn rendered_whitespace_names_are_distinct_without_changing_selected_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("a b")).unwrap();
        fs::create_dir(temp.path().join("a  b")).unwrap();
        let listing = list(temp.path()).unwrap();
        assert!(listing.entries.iter().all(|entry| entry.directory));
        let screen = crate::ui::render(
            &view(temp.path(), &listing, 0, None),
            crate::ui::Theme {
                color: false,
                unicode: false,
                width: 100,
            },
        );
        assert!(screen.contains("a\\u{20}b"));
        assert!(screen.contains("a\\u{20}\\u{20}b"));
        assert!(
            listing
                .entries
                .iter()
                .any(|entry| entry.path == temp.path().join("a b"))
        );
        assert!(
            listing
                .entries
                .iter()
                .any(|entry| entry.path == temp.path().join("a  b"))
        );
    }
    #[test]
    fn escaped_labels_are_injective_and_all_failed_entries_do_not_look_empty() {
        assert_ne!(visible_filename("a b"), visible_filename(r"a\u{20}b"));
        assert_ne!(visible_filename("a b"), visible_filename("a\u{a0}b"));
        assert!(!visible_filename("line\n\x1b[2J").contains('\x1b'));
        let listing = collect_entries(std::iter::once(Err(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        ))));
        let screen = crate::ui::render(
            &view(Path::new("/fixture"), &listing, 0, None),
            crate::ui::Theme {
                color: false,
                unicode: false,
                width: 88,
            },
        );
        assert!(screen.contains("could not be inspected"));
        assert!(!screen.contains("This folder is empty"));
        let temp = tempfile::tempdir().unwrap();
        assert!(list(&temp.path().join("missing")).is_err());
    }
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
