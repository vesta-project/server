//! Projection of the authorized branding icon onto Minecraft's fixed game-root leaf.
//! Only the stopped process owner reconciles it, including on autonomous restart.
use crate::{
    Result, StateDirectory, StateError,
    database::connect,
    directory::{open_execution_file, open_regular, reject_linked_directories, require_directory},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

pub const FILE_NAME: &str = "server-icon.png";
pub const BACKUP_NAME: &str = "server-icon.vesta-backup.png";
pub const MAX_PNG_BYTES: usize = 17 * 1024;
pub const MAX_GAME_ICON_BYTES: usize = 256 * 1024;
pub const STAGE_PREFIX: &str = ".vesta-stage-icon-";

/// The caller derives this from a validated document and canonical FileBrowser root
/// in the same transaction as the document and its audit.
pub fn store(c: &Connection, game: Option<&Path>, png: Option<&[u8]>) -> Result<()> {
    store_projection(c, game, png, false)
}

/// An explicit launcher upload may replace a user icon once, after preserving it.
pub fn store_branding(c: &Connection, game: &Path, png: Option<&[u8]>, explicit: bool) -> Result<()> {
    store_projection(c, Some(game), png, explicit)
}

fn store_projection(c: &Connection, game: Option<&Path>, png: Option<&[u8]>, replace: bool) -> Result<()> {
    ensure_table(c)?;
    let game = game
        .map(|path| path.to_str().ok_or_else(unavailable))
        .transpose()?;
    c.execute(
        "INSERT INTO server_icon(singleton,game,enabled,png,replace_user) VALUES(1,?,?,?,?)
         ON CONFLICT(singleton) DO UPDATE SET
         game=coalesce(excluded.game,game),enabled=excluded.enabled,png=excluded.png,
         replace_user=CASE WHEN excluded.png IS NULL THEN 0
             WHEN excluded.replace_user=1 THEN 1
             WHEN game=excluded.game AND png IS excluded.png THEN replace_user ELSE 0 END,
         owner=CASE WHEN excluded.game IS NULL OR game=excluded.game THEN owner END,
         next_owner=CASE WHEN excluded.game IS NULL OR game=excluded.game THEN next_owner END,
         stage=CASE WHEN excluded.game IS NULL OR game=excluded.game THEN stage END",
        params![game, game.is_some(), png, replace && png.is_some()],
    )?;
    Ok(())
}

fn ensure_table(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS server_icon (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            game TEXT, enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
            png BLOB CHECK(length(png)<=17408),
            owner TEXT, next_owner TEXT, stage TEXT,
            replace_user INTEGER NOT NULL DEFAULT 0 CHECK(replace_user IN (0,1))
        ) STRICT;",
    )?;
    Ok(())
}

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    let needs_column: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='server_icon')
         AND NOT EXISTS(SELECT 1 FROM pragma_table_info('server_icon') WHERE name='replace_user')",
        [],
        |row| row.get(0),
    )?;
    if needs_column {
        c.execute_batch("ALTER TABLE server_icon ADD COLUMN replace_user INTEGER NOT NULL DEFAULT 0 CHECK(replace_user IN (0,1))")?;
    }
    Ok(())
}

/// Read a user-supplied icon, excluding both possible projected file identities.
pub fn user_icon(db: &crate::StateDatabase, game: &Path) -> Result<Option<Vec<u8>>> {
    reject_linked_directories(game, "state_server_icon_unavailable")?;
    require_directory(game)?;
    let game = fs::canonicalize(game)?;
    let game_path = game.to_str().ok_or_else(unavailable)?;
    db.read(|c| {
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='server_icon' AND type='table')",
            [],
            |row| row.get(0),
        )?;
        let owned = if exists {
            c.query_row(
                "SELECT owner,next_owner,replace_user FROM server_icon WHERE singleton=1 AND game=?",
                [game_path],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, bool>(2)?)),
            ).optional()?.unwrap_or_default()
        } else {
            (None, None, false)
        };
        let Current::Present(icon) = read_icon_bounded(&game.join(FILE_NAME), MAX_GAME_ICON_BYTES)? else {
            return Ok(None);
        };
        if owned.2 || owned.0.as_ref() == Some(&icon.id) || owned.1.as_ref() == Some(&icon.id) {
            return Ok(None);
        }
        // Check the signature and dimensions without excluding indexed or interlaced PNGs.
        let header = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x40\0\0\0\x40";
        Ok((icon.bytes.len() >= 45 && icon.bytes.starts_with(header)).then_some(icon.bytes))
    })
}

struct Projection {
    game: String,
    png: Option<Vec<u8>>,
    owner: Option<String>,
    next_owner: Option<String>,
    stage: Option<String>,
    replace_user: bool,
}

/// Caller holds stopped ownership. Failures leave the presentation retryable.
pub fn reconcile(state: &Path, game: &Path) -> Result<()> {
    let state = StateDirectory::new(state, Some(game))?;
    if !state.database_file().try_exists()? {
        return Ok(());
    }
    let c = connect(&state, false)?;
    // Keep the process owner's stop/start loop responsive during other writers.
    c.busy_timeout(std::time::Duration::from_millis(25))?;
    let initialized: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='server_icon')",
        [],
        |row| row.get(0),
    )?;
    if !initialized {
        return Ok(());
    }
    let Some(row) = c.query_row(
        "SELECT game,png,owner,next_owner,stage,replace_user FROM server_icon
         WHERE singleton=1 AND enabled=1 AND (png IS NOT NULL OR owner IS NOT NULL OR stage IS NOT NULL)",
        [],
        |row| Ok(Projection {
            game: row.get(0)?, png: row.get(1)?, owner: row.get(2)?,
            next_owner: row.get(3)?, stage: row.get(4)?, replace_user: row.get(5)?,
        }),
    ).optional()? else {
        return Ok(());
    };
    reject_linked_directories(game, "state_server_icon_unavailable")?;
    require_directory(game)?;
    let game = fs::canonicalize(game)?;
    if game != Path::new(&row.game) {
        return Ok(());
    }
    let target = game.join(FILE_NAME);
    // A failure before the move can leave the recorded stage behind. Retire
    // only the verified file, keeping either recorded target identity valid.
    if let Some(stage) = &row.stage
        && let Ok(id) = uuid::Uuid::parse_str(stage)
    {
        let stage = stage_path(&game, id);
        if let Current::Present(icon) = read_icon(&stage)?
            && Some(&icon.id) == row.next_owner.as_ref()
        {
            fs::remove_file(stage)?;
        }
    }
    let current = read_icon_bounded(&target, MAX_GAME_ICON_BYTES)?;
    let manual = match &current {
        Current::Other => return Ok(()),
        Current::Present(icon)
            if Some(&icon.id) != row.owner.as_ref()
                && Some(&icon.id) != row.next_owner.as_ref() => {
            if !row.replace_user {
                return Ok(());
            }
            Some(icon)
        }
        _ => None,
    };
    let current_id = match &current {
        Current::Present(icon) => Some(icon.id.as_str()),
        Current::Absent => None,
        Current::Other => unreachable!("checked above"),
    };
    if let Some(png) = row.png.as_ref() {
        if manual.is_some() || !matches!(&current, Current::Present(icon) if icon.bytes == *png) {
            install(&c, &game, &target, current_id, row.owner.as_deref(), manual, png)?;
            return Ok(());
        }
    } else if current_id.is_some() {
        return apply(&c, &game, None, || {
            if !unchanged(&target, current_id)? {
                return Ok((current_id, false));
            }
            fs::remove_file(&target)?;
            sync_game(&game)?;
            Ok((None, true))
        });
    }
    let owner = row.png.as_ref().and(current_id);
    if owner != row.owner.as_deref() || row.stage.is_some() || row.replace_user {
        apply(&c, &game, row.png.as_deref(), || {
            Ok((owner, unchanged(&target, current_id)?))
        })?;
    }
    Ok(())
}

fn install(
    c: &Connection,
    game: &Path,
    target: &Path,
    old: Option<&str>,
    owner: Option<&str>,
    manual: Option<&Icon>,
    png: &[u8],
) -> Result<()> {
    let stage_id = uuid::Uuid::new_v4();
    let stage = stage_path(game, stage_id);
    let mut file = open_regular(&stage, true, false)?;
    let result = (|| {
        file.write_all(png)?;
        file.sync_all()?;
        drop(file);
        let Current::Present(staged) = read_icon(&stage)? else {
            return Err(unavailable());
        };
        if staged.bytes != png {
            return Err(unavailable());
        }
        let next = staged.id;
        // Record both file identities before the atomic move. A crash either
        // side of the move leaves it possible to recognize the owned file.
        record(c, game, owner, Some(&next), Some(&stage_id.to_string()))?;
        apply(c, game, Some(png), || {
            if !unchanged(target, old)? {
                return Ok((owner, false));
            }
            if let Some(manual) = manual {
                backup(game, &manual.bytes)?;
                if !unchanged(target, old)? {
                    return Ok((owner, false));
                }
            }
            if old.is_some() {
                fs::rename(&stage, target)?;
            } else {
                rename_absent(&stage, target)?;
            }
            sync_game(game)?;
            Ok((Some(next.as_str()), true))
        })
    })();
    if fs::symlink_metadata(&stage).is_ok() {
        let _ = fs::remove_file(&stage);
    }
    result
}

/// Exclude launcher file mutations through their normal SQLite effect transaction,
/// and recheck that the authorized policy has not changed since staging.
fn apply<'a>(
    c: &Connection,
    game: &Path,
    png: Option<&[u8]>,
    effect: impl FnOnce() -> Result<(Option<&'a str>, bool)>,
) -> Result<()> {
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    let allowed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM server_icon
            WHERE singleton=1 AND enabled=1 AND game=? AND png IS ?)
         AND NOT EXISTS(SELECT 1 FROM file_intents
            WHERE status IN ('pending','recovery-required') AND path=? COLLATE NOCASE)",
        params![game.to_str().ok_or_else(unavailable)?, png, FILE_NAME],
        |row| row.get(0),
    )?;
    if allowed {
        reject_linked_directories(game, "state_server_icon_unavailable")?;
        require_directory(game)?;
        let (owner, applied) = effect()?;
        record(&tx, game, owner, None, None)?;
        if applied {
            tx.execute("UPDATE server_icon SET replace_user=0 WHERE singleton=1", [])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn record(
    c: &Connection,
    game: &Path,
    owner: Option<&str>,
    next_owner: Option<&str>,
    stage: Option<&str>,
) -> Result<()> {
    if c.execute(
        "UPDATE server_icon SET owner=?,next_owner=?,stage=? WHERE singleton=1 AND game=?",
        params![
            owner,
            next_owner,
            stage,
            game.to_str().ok_or_else(unavailable)?
        ],
    )? != 1
    {
        return Err(unavailable());
    }
    Ok(())
}

fn backup(game: &Path, bytes: &[u8]) -> Result<()> {
    let id = uuid::Uuid::new_v4();
    let stage = stage_path(game, id);
    let result = (|| {
        let mut file = open_regular(&stage, true, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match rename_absent(&stage, &game.join(BACKUP_NAME)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                rename_absent(&stage, &game.join(format!("server-icon.vesta-backup-{id}.png")))?;
            }
            Err(e) => return Err(e.into()),
        }
        sync_game(game)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result
}

fn stage_path(game: &Path, id: uuid::Uuid) -> std::path::PathBuf {
    game.join(format!("{STAGE_PREFIX}{id}.tmp"))
}

fn unchanged(target: &Path, expected: Option<&str>) -> Result<bool> {
    Ok(match read_icon_bounded(target, MAX_GAME_ICON_BYTES)? {
        Current::Absent => expected.is_none(),
        Current::Present(icon) => Some(icon.id.as_str()) == expected,
        Current::Other => false,
    })
}

struct Icon {
    bytes: Vec<u8>,
    id: String,
}
enum Current {
    Absent,
    Present(Icon),
    Other,
}

fn read_icon(path: &Path) -> Result<Current> {
    read_icon_bounded(path, MAX_PNG_BYTES)
}

fn read_icon_bounded(path: &Path, limit: usize) -> Result<Current> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Current::Absent),
        Err(err) => return Err(err.into()),
        Ok(metadata) if metadata.len() > limit as u64 => return Ok(Current::Other),
        Ok(_) => {}
    }
    let Ok(file) = open_execution_file(path) else {
        return Ok(Current::Other);
    };
    let mut bytes = Vec::new();
    (&file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Ok(Current::Other);
    }
    let id = fingerprint(&file, &bytes)?;
    Ok(Current::Present(Icon { bytes, id }))
}

fn fingerprint(file: &File, bytes: &[u8]) -> Result<String> {
    let metadata = file.metadata()?;
    #[cfg(target_os = "linux")]
    let id = {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{}:{:?}",
            metadata.dev(),
            metadata.ino(),
            metadata.created().ok()
        )
    };
    #[cfg(windows)]
    let id = crate::windows_security::regular_file_id(file)?;
    #[cfg(not(any(windows, target_os = "linux")))]
    let id = return Err(unavailable());
    Ok(format!(
        "{id}:{:?}:{}",
        metadata.modified().ok(),
        hex::encode(Sha256::digest(bytes))
    ))
}

fn sync_game(game: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(game)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = game;
    Ok(())
}

fn unavailable() -> StateError {
    StateError::new("state_server_icon_unavailable")
}

// A no-replace move also keeps the staged file single-linked across a crash.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn rename_absent(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both absolute paths are NUL-terminated for this synchronous call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_absent(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: both NUL-terminated paths live for the synchronous call.
    let result = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rename_absent(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StateDatabase, SystemClock};
    use std::{path::PathBuf, sync::Arc};

    struct Fixture {
        _temp: tempfile::TempDir,
        db: StateDatabase,
        game: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let game = temp.path().join("game");
            fs::create_dir(&game).unwrap();
            let game = fs::canonicalize(game).unwrap();
            let state = StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
            let db = StateDatabase::open(state, Arc::new(SystemClock)).unwrap();
            Self {
                _temp: temp,
                db,
                game,
            }
        }
        fn set(&self, png: Option<&[u8]>) {
            self.db.write(|c| store(c, Some(&self.game), png)).unwrap();
        }
        fn reconcile(&self) {
            reconcile(self.db.directory().path(), &self.game).unwrap();
        }
        fn target(&self) -> PathBuf {
            self.game.join(FILE_NAME)
        }
    }

    #[test]
    fn only_the_owned_icon_is_updated_or_removed() {
        let f = Fixture::new();
        let target = f.target();
        f.set(Some(b"first"));
        f.reconcile();
        assert_eq!(fs::read(&target).unwrap(), b"first");
        f.set(Some(b"second"));
        f.reconcile();
        assert_eq!(fs::read(&target).unwrap(), b"second");
        f.set(None);
        f.reconcile();
        assert!(!target.exists());

        fs::write(&target, b"manual").unwrap();
        f.set(Some(b"configured"));
        f.reconcile();
        f.set(None);
        f.reconcile();
        assert_eq!(fs::read(&target).unwrap(), b"manual");

        fs::remove_file(&target).unwrap();
        f.set(Some(b"same bytes"));
        f.reconcile();
        // A replacement file with identical bytes is still a different file.
        let manual = f.game.join("manual.png");
        fs::write(&manual, b"same bytes").unwrap();
        fs::rename(manual, &target).unwrap();
        f.set(None);
        f.reconcile();
        assert_eq!(fs::read(&target).unwrap(), b"same bytes");
    }

    #[test]
    fn explicit_branding_preserves_manual_icons_before_replacing_them() {
        let f = Fixture::new();
        fs::write(f.target(), b"manual first").unwrap();
        let upload = |bytes: &[u8]| {
            f.db.write(|c| store_branding(c, &f.game, Some(bytes), true)).unwrap();
        };
        upload(b"branding first");
        f.reconcile();
        assert_eq!(fs::read(f.target()).unwrap(), b"branding first");
        assert_eq!(fs::read(f.game.join(BACKUP_NAME)).unwrap(), b"manual first");

        fs::write(f.target(), b"manual second").unwrap();
        f.reconcile();
        assert_eq!(fs::read(f.target()).unwrap(), b"manual second");
        upload(b"branding second");
        f.reconcile();
        assert_eq!(fs::read(f.target()).unwrap(), b"branding second");
        assert_eq!(fs::read(f.game.join(BACKUP_NAME)).unwrap(), b"manual first");
        assert!(fs::read_dir(&f.game).unwrap().flatten().any(|entry| {
            entry.file_name().to_string_lossy().starts_with("server-icon.vesta-backup-")
                && fs::read(entry.path()).unwrap() == b"manual second"
        }));
    }

    #[test]
    fn interrupted_publish_retains_ownership_on_both_sides_of_rename() {
        for published in [false, true] {
            let f = Fixture::new();
            f.set(Some(b"first"));
            f.reconcile();
            let target = f.target();
            let Current::Present(first) = read_icon(&target).unwrap() else {
                panic!("missing icon")
            };
            let stage_id = uuid::Uuid::new_v4();
            let stage = stage_path(&f.game, stage_id);
            fs::write(&stage, b"second").unwrap();
            let Current::Present(next) = read_icon(&stage).unwrap() else {
                panic!("missing stage")
            };
            f.set(Some(b"second"));
            f.db.write(|c| {
                record(
                    c,
                    &f.game,
                    Some(&first.id),
                    Some(&next.id),
                    Some(&stage_id.to_string()),
                )
            })
            .unwrap();
            if published {
                fs::rename(&stage, &target).unwrap();
            }
            f.reconcile();
            assert!(!stage.exists());
            assert_eq!(fs::read(&target).unwrap(), b"second");
            f.set(None);
            f.reconcile();
            assert!(!target.exists());
        }
    }

    #[test]
    fn hard_link_and_disabled_projection_are_preserved() {
        let f = Fixture::new();
        let other = f.game.join("other.png");
        fs::write(&other, b"owner").unwrap();
        fs::hard_link(&other, f.target()).unwrap();
        f.set(Some(b"first"));
        f.reconcile();
        assert_eq!(fs::read(f.target()).unwrap(), b"owner");
        fs::remove_file(f.target()).unwrap();
        f.reconcile();
        f.db.write(|c| store(c, None, None)).unwrap();
        f.reconcile();
        assert_eq!(fs::read(f.target()).unwrap(), b"first");
        f.set(None);
        f.reconcile();
        assert!(!f.target().exists());
    }
}
