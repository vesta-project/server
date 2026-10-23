//! Owned temporary game and state trees only.
use super::*;
use crate::identity::{DeviceId, PrincipalId};
use rusqlite::params;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tempfile::TempDir;
use vesta_state::{Clock, StateDatabase, StateDirectory, audit::AuditLog};

const START: i64 = 1_791_115_200_000;

struct SteppedClock(AtomicI64);
impl Clock for SteppedClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _temp: TempDir,
    game: PathBuf,
    clock: Arc<SteppedClock>,
    db: StateDatabase,
    owner: Subject,
    files: FileBrowser,
}
impl Fixture {
    fn open() -> Self {
        Self::with_policy(TrashPolicy::default())
    }
    fn with_policy(policy: TrashPolicy) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(game.join("config")).unwrap();
        let directory =
            StateDirectory::new(temp.path().join("private-state"), Some(&game)).unwrap();
        let clock = Arc::new(SteppedClock(AtomicI64::new(START)));
        let db = StateDatabase::open(directory, clock.clone()).unwrap();
        let owner = seed(&db, true);
        let files = FileBrowser::open(db.clone(), &game, &[])
            .unwrap()
            .with_trash_policy(policy);
        Self {
            _temp: temp,
            game,
            clock,
            db,
            owner,
            files,
        }
    }
    fn path(text: &str) -> RelativePath {
        RelativePath::parse(text).unwrap()
    }
    fn put(&self, text: &str, bytes: &[u8]) {
        fs::write(self.game.join(text), bytes).unwrap();
    }
    fn get(&self, text: &str) -> Option<Vec<u8>> {
        fs::read(self.game.join(text)).ok()
    }
    fn grant(&self, subject: &Subject, capability: Capability, scope: &str) {
        self.db.write(|c| {
            c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,?,'allow','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), capability.id(), scope, START])?;
            Ok(())
        }).unwrap();
    }
    fn trash_rows(&self) -> i64 {
        self.db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM file_trash", [], |row| row.get(0))?))
            .unwrap()
    }
    fn trash_files(&self) -> usize {
        fs::read_dir(self.db.directory().path().join("trash"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    }
    fn stages(&self, directory: &str) -> usize {
        fs::read_dir(self.game.join(directory))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".vesta-stage-")
            })
            .count()
    }
    fn audit_actions(&self) -> Vec<String> {
        AuditLog::new(self.db.clone())
            .unwrap()
            .page(0, 200)
            .unwrap()
            .into_iter()
            .map(|entry| entry.action)
            .collect()
    }
}
fn seed(db: &StateDatabase, owner: bool) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)",
            params![principal.as_str(), db.server_id(), START, START])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)",
            params![device.as_str(), db.server_id(), principal.as_str(), hex::encode(Sha256::digest(device.as_str().as_bytes())), START])?;
        if owner {
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), principal.as_str(), START])?;
        }
        Ok(())
    }).unwrap();
    Subject { principal, device }
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
#[track_caller]
fn refused<T: std::fmt::Debug>(result: Result<T>, code: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), code),
        Ok(value) => panic!("expected {code}, produced {value:?}"),
    }
}
fn matches(bytes: &[u8]) -> WritePrecondition {
    WritePrecondition::Matches(sha(bytes))
}

#[test]
fn creation_requires_absence_and_never_replaces() {
    let f = Fixture::open();
    let path = Fixture::path("config/new.toml");
    let change = f
        .files
        .write(&f.owner, &path, &WritePrecondition::Absent, b"fresh")
        .unwrap();
    assert_eq!(change.sha256, sha(b"fresh"));
    assert_eq!(change.size, 5);
    assert_eq!(change.trash, None);
    assert_eq!(f.get("config/new.toml").unwrap(), b"fresh");

    match f
        .files
        .write(&f.owner, &path, &WritePrecondition::Absent, b"second")
        .unwrap_err()
    {
        FileError::Conflict(current) => assert_eq!(current, Some(sha(b"fresh"))),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(f.get("config/new.toml").unwrap(), b"fresh");
    assert_eq!(f.stages("config"), 0);
    assert_eq!(f.trash_rows(), 0);
    assert!(f.audit_actions().contains(&"files.write".to_owned()));
}

#[test]
fn overwrites_need_the_current_hash_and_keep_a_recoverable_copy() {
    let f = Fixture::open();
    f.put("config/app.toml", b"version one");
    let path = Fixture::path("config/app.toml");

    match f
        .files
        .write(&f.owner, &path, &matches(b"stale"), b"version two")
        .unwrap_err()
    {
        FileError::Conflict(current) => assert_eq!(current, Some(sha(b"version one"))),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(f.get("config/app.toml").unwrap(), b"version one");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));

    let change = f
        .files
        .write(&f.owner, &path, &matches(b"version one"), b"version two")
        .unwrap();
    assert_eq!(f.get("config/app.toml").unwrap(), b"version two");
    let id = change.trash.unwrap();
    let entries = f.files.trash_entries(&f.owner).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].path, path);
    assert_eq!(entries[0].sha256, sha(b"version one"));
    assert_eq!(entries[0].expires_at, START + 7 * 24 * 60 * 60 * 1_000);
    assert_eq!(f.stages("config"), 0);
}

#[test]
fn deletes_go_to_the_trash_and_restore_only_into_an_empty_path() {
    let f = Fixture::open();
    f.put("config/old.json", b"keep me");
    let path = Fixture::path("config/old.json");
    refused(
        f.files.delete(&f.owner, &path, &sha(b"other")),
        "file_conflict",
    );
    assert!(f.get("config/old.json").is_some());

    let entry = f.files.delete(&f.owner, &path, &sha(b"keep me")).unwrap();
    assert!(f.get("config/old.json").is_none());
    assert_eq!(entry.sha256, sha(b"keep me"));

    f.put("config/old.json", b"newer");
    refused(f.files.restore(&f.owner, &entry.id), "file_conflict");
    assert_eq!(f.get("config/old.json").unwrap(), b"newer");
    fs::remove_file(f.game.join("config/old.json")).unwrap();

    let restored = f.files.restore(&f.owner, &entry.id).unwrap();
    assert_eq!(restored.sha256, sha(b"keep me"));
    assert_eq!(f.get("config/old.json").unwrap(), b"keep me");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
    refused(f.files.restore(&f.owner, &entry.id), "file_not_found");
    refused(f.files.restore(&f.owner, "not-an-id"), "file_not_found");
    let actions = f.audit_actions();
    for action in ["files.delete", "files.restore"] {
        assert!(actions.contains(&action.to_owned()), "{action}");
    }
}

#[test]
fn each_mutation_requires_its_own_scoped_capability() {
    let f = Fixture::open();
    f.put("config/app.toml", b"current");
    let stranger = seed(&f.db, false);
    let path = Fixture::path("config/app.toml");
    let fresh = Fixture::path("config/fresh.toml");
    refused(
        f.files
            .write(&stranger, &fresh, &WritePrecondition::Absent, b"x"),
        "forbidden",
    );
    f.grant(&stranger, Capability::FilesCreate, "config/**");
    f.files
        .write(&stranger, &fresh, &WritePrecondition::Absent, b"x")
        .unwrap();
    // Creating is not overwriting or deleting, and scope is honored.
    refused(
        f.files.write(&stranger, &path, &matches(b"current"), b"y"),
        "forbidden",
    );
    refused(
        f.files.delete(&stranger, &path, &sha(b"current")),
        "forbidden",
    );
    refused(
        f.files.write(
            &stranger,
            &Fixture::path("elsewhere.txt"),
            &WritePrecondition::Absent,
            b"x",
        ),
        "forbidden",
    );
    assert_eq!(f.get("config/app.toml").unwrap(), b"current");
    assert!(f.get("elsewhere.txt").is_none());
    assert_eq!(f.stages("config"), 0);
}

#[test]
fn protected_missing_and_linked_targets_refuse() {
    let f = Fixture::open();
    refused(RelativePath::parse(".vesta/state"), "file_path_protected");
    refused(
        f.files.write(
            &f.owner,
            &Fixture::path("missing/dir.txt"),
            &WritePrecondition::Absent,
            b"x",
        ),
        "file_not_found",
    );
    fs::create_dir(f.game.join("config/sub")).unwrap();
    refused(
        f.files
            .delete(&f.owner, &Fixture::path("config/sub"), &sha(b"")),
        "file_not_regular",
    );
    #[cfg(windows)]
    {
        let outside = f._temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(f.game.join("linked"))
            .arg(&outside)
            .status()
            .unwrap();
        assert!(status.success());
        refused(
            f.files.write(
                &f.owner,
                &Fixture::path("linked/escape.txt"),
                &WritePrecondition::Absent,
                b"x",
            ),
            "file_path_linked",
        );
        assert!(!outside.join("escape.txt").exists());
        fs::remove_dir(f.game.join("linked")).unwrap();
    }
}

#[test]
fn size_and_trash_capacity_bounds_refuse_without_losing_data() {
    let f = Fixture::with_policy(TrashPolicy::new(8, 100, Duration::from_secs(60), 1).unwrap());
    f.put("config/a.txt", b"0123456789");
    let path = Fixture::path("config/a.txt");
    refused(
        f.files.write(
            &f.owner,
            &Fixture::path("config/huge.bin"),
            &WritePrecondition::Absent,
            &vec![0; MAX_WRITE_BYTES + 1],
        ),
        "file_too_large",
    );
    // Ten bytes cannot fit in an eight-byte trash: the overwrite refuses intact.
    refused(
        f.files
            .write(&f.owner, &path, &matches(b"0123456789"), b"new"),
        "file_trash_full",
    );
    assert_eq!(f.get("config/a.txt").unwrap(), b"0123456789");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));

    f.put("config/b.txt", b"small");
    f.files
        .delete(&f.owner, &Fixture::path("config/b.txt"), &sha(b"small"))
        .unwrap();
    f.put("config/c.txt", b"tiny");
    // The entry ceiling is one.
    refused(
        f.files
            .delete(&f.owner, &Fixture::path("config/c.txt"), &sha(b"tiny")),
        "file_trash_full",
    );
    assert!(f.get("config/c.txt").is_some());
    for (bytes, percent, retention, entries) in [
        (0, 10, 1, 1),
        (1, 0, 1, 1),
        (1, 101, 1, 1),
        (1, 10, 0, 1),
        (1, 10, 1, 0),
    ] {
        refused(
            TrashPolicy::new(bytes, percent, Duration::from_secs(retention), entries),
            "file_limit_invalid",
        );
    }
}

#[test]
fn expired_trash_is_purged_with_its_copies() {
    let f = Fixture::with_policy(
        TrashPolicy::new(1_024 * 1_024, 100, Duration::from_secs(60), 10).unwrap(),
    );
    f.put("config/a.txt", b"a");
    f.files
        .delete(&f.owner, &Fixture::path("config/a.txt"), &sha(b"a"))
        .unwrap();
    assert_eq!(f.files.purge_expired_trash().unwrap(), 0);
    f.clock.0.store(START + 60_000, Ordering::SeqCst);
    assert_eq!(f.files.purge_expired_trash().unwrap(), 1);
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[test]
fn an_unrecorded_change_is_reversed_from_its_trash_copy() {
    let f = Fixture::open();
    f.put("config/app.toml", b"original");
    let path = Fixture::path("config/app.toml");
    let change = f
        .files
        .write(&f.owner, &path, &matches(b"original"), b"replacement")
        .unwrap();
    // Simulate the commit failing after the rename: settle must restore the old bytes.
    let target = f.game.join("config/app.toml");
    let canonical = fs::canonicalize(&target).unwrap();
    let result: Result<()> = f.files.settle(
        Err(FileError::State(vesta_state::StateError::new(
            "state_commit_failed",
        ))),
        Some(Applied {
            trash: change.trash.clone(),
            effect: Some(sha(b"replacement")),
        }),
        &canonical,
    );
    refused(result, "state_commit_failed");
    assert_eq!(f.get("config/app.toml").unwrap(), b"original");

    // A created file is removed again.
    let created = f.game.join("config/created.toml");
    fs::write(&created, b"x").unwrap();
    let result: Result<()> = f.files.settle(
        Err(FileError::Unavailable),
        Some(Applied {
            trash: None,
            effect: Some(sha(b"x")),
        }),
        &created,
    );
    refused(result, "file_unavailable");
    assert!(!created.exists());
}

#[test]
fn invalid_preconditions_refuse_before_any_staging() {
    let f = Fixture::open();
    f.put("config/app.toml", b"v");
    for bad in ["", "ABC", &"g".repeat(64), &"A".repeat(64)] {
        refused(
            f.files.write(
                &f.owner,
                &Fixture::path("config/app.toml"),
                &WritePrecondition::Matches(bad.to_owned()),
                b"x",
            ),
            "file_path_invalid",
        );
    }
    assert_eq!(f.stages("config"), 0);
    assert_eq!(f.get("config/app.toml").unwrap(), b"v");
}

#[test]
fn compensation_refuses_to_clobber_an_unowned_effect() {
    let f = Fixture::open();
    f.put("config/app.toml", b"original");
    let change = f
        .files
        .write(
            &f.owner,
            &Fixture::path("config/app.toml"),
            &matches(b"original"),
            b"replacement",
        )
        .unwrap();
    f.put("config/app.toml", b"later successful edit");
    let result: Result<()> = f.files.settle(
        Err(FileError::Unavailable),
        Some(Applied {
            trash: change.trash,
            effect: Some(sha(b"replacement")),
        }),
        &f.game.join("config/app.toml"),
    );
    refused(result, "file_recovery_required");
    assert_eq!(f.get("config/app.toml").unwrap(), b"later successful edit");
    assert_eq!((f.trash_rows(), f.trash_files()), (1, 1));
    let result: Result<()> = f.files.settle(
        Err(FileError::Unavailable),
        Some(Applied {
            trash: None,
            effect: Some(sha(b"created")),
        }),
        &f.game.join("config/app.toml"),
    );
    refused(result, "file_recovery_required");
    assert_eq!(f.get("config/app.toml").unwrap(), b"later successful edit");
}

#[test]
fn separate_browser_instances_share_admission_through_compensation() {
    let f = Fixture::open();
    let second = FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
    let one = mutation_admission(f.files.db.directory().path()).unwrap();
    let two = mutation_admission(second.db.directory().path()).unwrap();
    assert!(Arc::ptr_eq(&one, &two));
    let guard = one.lock().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let subject = f.owner.clone();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx
            .send(second.write(
                &subject,
                &Fixture::path("config/new.txt"),
                &WritePrecondition::Absent,
                b"new",
            ))
            .unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
    let target = f.game.join("config/failed.txt");
    fs::write(&target, b"failed").unwrap();
    let result: Result<()> = f.files.settle(
        Err(FileError::Unavailable),
        Some(Applied {
            trash: None,
            effect: Some(sha(b"failed")),
        }),
        &target,
    );
    refused(result, "file_unavailable");
    drop(guard);
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    assert_eq!(f.get("config/new.txt").unwrap(), b"new");
}

fn status(f: &Fixture, id: &str) -> String {
    f.db.read(|c| {
        Ok(
            c.query_row("SELECT status FROM file_trash WHERE id=?", [id], |row| {
                row.get(0)
            })?,
        )
    })
    .unwrap()
}

#[test]
fn durable_pending_provenance_precedes_effect_and_reconciles_conservatively() {
    let mut f = Fixture::open();
    let mut ids = Vec::new();
    for (name, effect) in [
        ("before.txt", Some(sha(b"new"))),
        ("after.txt", Some(sha(b"new"))),
        ("unknown.txt", Some(sha(b"new"))),
        ("deleted.txt", None),
    ] {
        let text = format!("config/{name}");
        f.put(&text, b"old");
        let reason = if effect.is_some() {
            Reason::Replace
        } else {
            Reason::Delete
        };
        let id = f
            .files
            .prepare_trash(
                &f.owner,
                &Fixture::path(&text),
                &sha(b"old"),
                reason,
                effect.as_deref(),
            )
            .unwrap();
        assert_eq!(status(&f, &id), "pending");
        assert_eq!(f.get(&text).unwrap(), b"old");
        let entry =
            f.db.read_with(|s| load_entry(s.connection(), f.db.server_id(), &id))
                .unwrap()
                .unwrap();
        assert_eq!(
            read_trash(
                &f.files.trash_dir().unwrap(),
                &trash_name(&id).unwrap(),
                &entry
            )
            .unwrap(),
            b"old"
        );
        ids.push(id);
    }
    f.put("config/after.txt", b"new");
    f.put("config/unknown.txt", b"foreign");
    fs::remove_file(f.game.join("config/deleted.txt")).unwrap();
    let directory = f.db.directory().clone();
    f.db.close().unwrap();
    f.db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    let reopened = FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
    assert_eq!(reopened.reconcile_pending().unwrap(), 4);
    assert_eq!(status(&f, &ids[0]), "recovery-required");
    assert_eq!(status(&f, &ids[1]), "complete");
    assert_eq!(status(&f, &ids[2]), "recovery-required");
    assert_eq!(status(&f, &ids[3]), "complete");
    assert_eq!((f.trash_rows(), f.trash_files()), (4, 4));
    assert_eq!(reopened.reconcile_pending().unwrap(), 0);
    refused(
        reopened.restore(&f.owner, &ids[0]),
        "file_recovery_required",
    );
    f.clock
        .0
        .store(START + 8 * 24 * 60 * 60 * 1000, Ordering::SeqCst);
    assert_eq!(reopened.purge_expired_trash().unwrap(), 2);
    assert_eq!((f.trash_rows(), f.trash_files()), (2, 2));
    assert_eq!(f.get("config/unknown.txt").unwrap(), b"foreign");
}

#[test]
fn failed_pending_insert_cleans_its_owned_copy() {
    let f = Fixture::open();
    f.put("config/app.toml", b"old");
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER reject_trash BEFORE INSERT ON file_trash BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(
        f.files
            .delete(&f.owner, &Fixture::path("config/app.toml"), &sha(b"old"))
            .is_err()
    );
    assert_eq!(f.get("config/app.toml").unwrap(), b"old");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[test]
fn purge_keeps_accounting_when_copy_removal_fails_then_retries() {
    let f = Fixture::open();
    f.put("config/a.txt", b"a");
    let entry = f
        .files
        .delete(&f.owner, &Fixture::path("config/a.txt"), &sha(b"a"))
        .unwrap();
    let copy =
        f.db.directory()
            .path()
            .join("trash")
            .join(format!("{}.bin", entry.id));
    let held = f.db.directory().path().join("trash").join("held.bin");
    fs::rename(&copy, &held).unwrap();
    fs::create_dir(&copy).unwrap();
    f.clock.0.store(entry.expires_at, Ordering::SeqCst);
    assert_eq!(f.files.purge_expired_trash().unwrap(), 0);
    assert_eq!(f.trash_rows(), 1);
    fs::remove_dir(&copy).unwrap();
    fs::rename(&held, &copy).unwrap();
    assert_eq!(f.files.purge_expired_trash().unwrap(), 1);
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[cfg(windows)]
#[test]
// Windows-only fixture cleanup restores its read-only attribute, not Unix mode bits.
#[allow(clippy::permissions_set_readonly_false)]
fn readonly_and_sharing_denied_targets_leave_no_orphan_copies() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::open();
    f.put("config/readonly.txt", b"old");
    let target = f.game.join("config/readonly.txt");
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions).unwrap();
    let path = Fixture::path("config/readonly.txt");
    assert!(f.files.delete(&f.owner, &path, &sha(b"old")).is_err());
    assert!(
        f.files
            .write(&f.owner, &path, &matches(b"old"), b"new")
            .is_err()
    );
    assert_eq!(f.get("config/readonly.txt").unwrap(), b"old");
    assert!(fs::metadata(&target).unwrap().permissions().readonly());
    assert_eq!(
        (f.trash_rows(), f.trash_files(), f.stages("config")),
        (0, 0, 0)
    );
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&target, permissions).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&target)
        .unwrap();
    assert!(f.files.delete(&f.owner, &path, &sha(b"old")).is_err());
    assert!(
        f.files
            .write(&f.owner, &path, &matches(b"old"), b"new")
            .is_err()
    );
    assert_eq!(
        (f.trash_rows(), f.trash_files(), f.stages("config")),
        (0, 0, 0)
    );
    drop(held);
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn short_leaf(path: &Path) -> Option<String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut output = vec![0_u16; 32768];
    // SAFETY: stable NUL-terminated input and correctly sized owned output.
    let written =
        unsafe { GetShortPathNameW(wide.as_ptr(), output.as_mut_ptr(), output.len() as u32) }
            as usize;
    if written == 0 || written >= output.len() {
        eprintln!(
            "8.3 alias assertion skipped: GetShortPathNameW failed: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    output.truncate(written);
    let short = PathBuf::from(std::ffi::OsString::from_wide(&output));
    let leaf = short.file_name()?.to_str()?.to_owned();
    if !leaf.contains('~') {
        eprintln!(
            "8.3 alias assertion skipped: owned temporary volume supplied no short leaf (8.3 names disabled or unavailable)"
        );
        return None;
    }
    Some(leaf)
}

#[cfg(windows)]
#[test]
fn existing_leaf_case_and_real_short_aliases_are_canonically_authorized() {
    let f = Fixture::open();
    f.put("config/CanonicalLongName.toml", b"secret");
    let stranger = seed(&f.db, false);
    let case = "config/CANONICALLONGNAME.TOML";
    f.grant(&stranger, Capability::FilesOverwrite, case);
    f.grant(&stranger, Capability::FilesDelete, case);
    refused(
        f.files.write(
            &stranger,
            &Fixture::path(case),
            &matches(b"secret"),
            b"oops",
        ),
        "forbidden",
    );
    refused(
        f.files
            .delete(&stranger, &Fixture::path(case), &sha(b"secret")),
        "forbidden",
    );
    if let Some(alias) = short_leaf(&f.game.join("config/CanonicalLongName.toml")) {
        let text = format!("config/{alias}");
        f.grant(&stranger, Capability::FilesOverwrite, &text);
        f.grant(&stranger, Capability::FilesDelete, &text);
        refused(
            f.files.write(
                &stranger,
                &Fixture::path(&text),
                &matches(b"secret"),
                b"oops",
            ),
            "forbidden",
        );
        refused(
            f.files
                .delete(&stranger, &Fixture::path(&text), &sha(b"secret")),
            "forbidden",
        );
    }
    f.put("wrapper-config.toml", b"protected");
    if let Some(alias) = short_leaf(&f.game.join("wrapper-config.toml")) {
        refused(
            f.files.write(
                &f.owner,
                &Fixture::path(&alias),
                &matches(b"protected"),
                b"oops",
            ),
            "file_path_protected",
        );
        refused(
            f.files
                .delete(&f.owner, &Fixture::path(&alias), &sha(b"protected")),
            "file_path_protected",
        );
    }
    assert_eq!(f.get("wrapper-config.toml").unwrap(), b"protected");
    assert_eq!(f.get("config/CanonicalLongName.toml").unwrap(), b"secret");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[cfg(unix)]
#[test]
fn replacement_and_restore_preserve_unix_mode_and_stage_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::open();
    let stage = Stage::write(&f.game.join("config"), b"private").unwrap();
    assert_eq!(
        fs::metadata(&stage.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(stage);
    f.put("config/mode.txt", b"old");
    let target = f.game.join("config/mode.txt");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let path = Fixture::path("config/mode.txt");
    f.files
        .write(&f.owner, &path, &matches(b"old"), b"new")
        .unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let entry = f.files.delete(&f.owner, &path, &sha(b"new")).unwrap();
    f.files.restore(&f.owner, &entry.id).unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn space_preflight_reserves_trash_volume_and_combined_footprints() {
    let reserve = SPACE_RESERVE_BYTES;
    let (target_need, state_need) = space_footprints(20, 30, false);
    assert_eq!((target_need, state_need), (20, 30));
    require_available(reserve + 20, target_need).unwrap();
    refused(
        require_available(reserve + 29, state_need),
        "file_insufficient_space",
    );
    let (target_need, state_need) = space_footprints(20, 30, true);
    assert_eq!((target_need, state_need), (50, 50));
    refused(
        require_available(reserve + 49, target_need),
        "file_insufficient_space",
    );
    require_available(reserve + 50, state_need).unwrap();
    refused(require_available(reserve - 1, 0), "file_insufficient_space");
}

#[test]
fn actual_copy_length_not_initial_metadata_controls_quota() {
    let f = Fixture::with_policy(TrashPolicy::new(8, 100, Duration::from_secs(60), 10).unwrap());
    f.db.read_with(|snapshot| {
        let connection = snapshot.connection();
        f.files.require_trash_capacity(connection, 1)?;
        let (digest, size) =
            copy_source(&mut std::io::Cursor::new(b"0123456789"), &mut Vec::new())?;
        assert_eq!(digest, sha(b"0123456789"));
        assert_eq!(size, 10);
        refused(
            f.files.require_trash_capacity(connection, size),
            "file_trash_full",
        );
        Ok::<_, FileError>(())
    })
    .unwrap();
}

#[test]
fn failed_completion_compensates_then_retires_pending_copy() {
    let f = Fixture::open();
    f.put("config/a.txt", b"old");
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER reject_file_audit BEFORE INSERT ON audit_events WHEN NEW.action LIKE 'files.%' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(
        f.files
            .write(
                &f.owner,
                &Fixture::path("config/a.txt"),
                &matches(b"old"),
                b"new"
            )
            .is_err()
    );
    assert_eq!(f.get("config/a.txt").unwrap(), b"old");
    assert_eq!(
        (f.trash_rows(), f.trash_files(), f.stages("config")),
        (0, 0, 0)
    );
    assert!(
        f.files
            .delete(&f.owner, &Fixture::path("config/a.txt"), &sha(b"old"))
            .is_err()
    );
    assert_eq!(f.get("config/a.txt").unwrap(), b"old");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[cfg(windows)]
#[test]
// Windows-only fixture cleanup restores its read-only attribute, not Unix mode bits.
#[allow(clippy::permissions_set_readonly_false)]
fn restore_preserves_recorded_readonly_without_leaking_a_stage() {
    let f = Fixture::open();
    f.put("config/attr.txt", b"old");
    let entry = f
        .files
        .delete(&f.owner, &Fixture::path("config/attr.txt"), &sha(b"old"))
        .unwrap();
    // Read-only targets cannot be deleted by this API; emulate a recorded original
    // attribute to exercise Windows restore without weakening target refusal semantics.
    f.db.write(|c| {
        c.execute("UPDATE file_trash SET readonly=1 WHERE id=?", [&entry.id])?;
        Ok(())
    })
    .unwrap();
    f.files.restore(&f.owner, &entry.id).unwrap();
    let target = f.game.join("config/attr.txt");
    assert!(fs::metadata(&target).unwrap().permissions().readonly());
    assert_eq!(f.stages("config"), 0);
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&target, permissions).unwrap();
}

#[test]
fn unrecorded_copies_still_consume_quota_without_being_deleted() {
    let f = Fixture::with_policy(TrashPolicy::new(8, 100, Duration::from_secs(60), 10).unwrap());
    let trash = f.files.trash_dir().unwrap();
    let name = trash_name(&"f".repeat(32)).unwrap();
    let mut copy = trash.create_new(&name).unwrap();
    copy.write_all(b"untracked").unwrap();
    copy.sync_all().unwrap();
    drop(copy);
    f.put("config/a.txt", b"a");
    refused(
        f.files
            .delete(&f.owner, &Fixture::path("config/a.txt"), &sha(b"a")),
        "file_trash_full",
    );
    assert_eq!(f.get("config/a.txt").unwrap(), b"a");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 1));
    assert_eq!(f.files.purge_expired_trash().unwrap(), 0);
    assert!(trash.open(&name).unwrap().is_some());
}

#[cfg(windows)]
#[test]
fn canceled_copy_with_sharing_denied_cleanup_is_accounted_and_retried() {
    let f = Fixture::open();
    f.put("config/a.txt", b"a");
    let id = f
        .files
        .prepare_trash(
            &f.owner,
            &Fixture::path("config/a.txt"),
            &sha(b"a"),
            Reason::Delete,
            None,
        )
        .unwrap();
    let held = f
        .files
        .trash_dir()
        .unwrap()
        .open(&trash_name(&id).unwrap())
        .unwrap()
        .unwrap();
    let failure: Result<()> = Err(FileError::Unavailable);
    assert!(f.files.retire_failed(&id, &failure).is_err());
    assert_eq!(status(&f, &id), "restored");
    assert_eq!((f.trash_rows(), f.trash_files()), (1, 1));
    drop(held);
    assert_eq!(f.files.purge_expired_trash().unwrap(), 1);
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[test]
fn failed_completion_commit_reverses_effect_with_durable_pending_copy() {
    let f = Fixture::open();
    f.put("config/a.txt", b"old");
    f.db.write(|c| {
        c.execute_batch("CREATE TABLE deferred_failure (principal TEXT REFERENCES principals(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_file_completion AFTER UPDATE OF status ON file_trash WHEN NEW.status='complete' BEGIN INSERT INTO deferred_failure VALUES ('missing-principal'); END;")?;
        Ok(())
    }).unwrap();
    let path = Fixture::path("config/a.txt");
    refused(
        f.files.write(&f.owner, &path, &matches(b"old"), b"new"),
        "state_commit_failed",
    );
    assert_eq!(f.get("config/a.txt").unwrap(), b"old");
    assert_eq!(
        (f.trash_rows(), f.trash_files(), f.stages("config")),
        (0, 0, 0)
    );
    refused(
        f.files.delete(&f.owner, &path, &sha(b"old")),
        "state_commit_failed",
    );
    assert_eq!(f.get("config/a.txt").unwrap(), b"old");
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[cfg(windows)]
#[test]
fn committed_restore_reports_success_and_keeps_denied_cleanup_for_retry() {
    let f = Fixture::open();
    f.put("config/a.txt", b"a");
    let entry = f
        .files
        .delete(&f.owner, &Fixture::path("config/a.txt"), &sha(b"a"))
        .unwrap();
    let held = f
        .files
        .trash_dir()
        .unwrap()
        .open(&trash_name(&entry.id).unwrap())
        .unwrap()
        .unwrap();
    f.files.restore(&f.owner, &entry.id).unwrap();
    assert_eq!(f.get("config/a.txt").unwrap(), b"a");
    assert_eq!(status(&f, &entry.id), "restored");
    assert_eq!((f.trash_rows(), f.trash_files()), (1, 1));
    drop(held);
    assert_eq!(f.files.purge_expired_trash().unwrap(), 1);
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[test]
fn mkdir_creates_one_level_and_audits_only_success() {
    let f = Fixture::open();
    let path = Fixture::path("config/new-directory");
    f.files.mkdir(&f.owner, &path).unwrap();
    assert!(f.game.join(path.as_str()).is_dir());
    refused(f.files.mkdir(&f.owner, &path), "file_conflict");
    f.put("config/existing.txt", b"untouched");
    refused(
        f.files
            .mkdir(&f.owner, &Fixture::path("config/existing.txt")),
        "file_conflict",
    );
    refused(
        f.files.mkdir(&f.owner, &Fixture::path("missing/child")),
        "file_not_found",
    );
    assert!(!f.game.join("missing").exists());
    assert_eq!(f.get("config/existing.txt").unwrap(), b"untouched");
    assert_eq!(
        f.audit_actions()
            .iter()
            .filter(|a| *a == "files.mkdir")
            .count(),
        1
    );
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn rename_moves_one_file_to_an_absent_name_and_audits_both_names() {
    let f = Fixture::open();
    f.put("config/source.txt", b"keep bytes");
    f.files
        .mkdir(&f.owner, &Fixture::path("destination"))
        .unwrap();
    let from = Fixture::path("config/source.txt");
    let to = Fixture::path("destination/moved.txt");
    let change = f
        .files
        .rename(&f.owner, &from, &to, &sha(b"keep bytes"))
        .unwrap();
    assert_eq!(
        change,
        FileChange {
            sha256: sha(b"keep bytes"),
            size: 10,
            trash: None
        }
    );
    assert!(f.get(from.as_str()).is_none());
    assert_eq!(f.get(to.as_str()).unwrap(), b"keep bytes");
    let metadata: String =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT metadata_json FROM audit_events WHERE action='files.rename'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(metadata.contains(from.as_str()));
    assert!(metadata.contains(to.as_str()));
    assert!(!metadata.contains(&f.game.to_string_lossy().to_string()));
    assert_eq!((f.trash_rows(), f.trash_files()), (0, 0));
}

#[test]
fn rename_refuses_stale_hash_existing_targets_and_missing_parents() {
    let f = Fixture::open();
    f.put("config/source.txt", b"source");
    f.put("config/existing.txt", b"target");
    fs::create_dir(f.game.join("config/existing-dir")).unwrap();
    let from = Fixture::path("config/source.txt");
    let fresh = Fixture::path("config/new.txt");
    match f
        .files
        .rename(&f.owner, &from, &fresh, &sha(b"stale"))
        .unwrap_err()
    {
        FileError::Conflict(current) => assert_eq!(current, Some(sha(b"source"))),
        error => panic!("unexpected {error:?}"),
    }
    for to in [
        "config/existing.txt",
        "config/existing-dir",
        "config/source.txt",
    ] {
        refused(
            f.files
                .rename(&f.owner, &from, &Fixture::path(to), &sha(b"source")),
            "file_conflict",
        );
    }
    refused(
        f.files.rename(
            &f.owner,
            &from,
            &Fixture::path("missing/file.txt"),
            &sha(b"source"),
        ),
        "file_not_found",
    );
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("config/absent.txt"),
            &fresh,
            &sha(b"source"),
        ),
        "file_not_found",
    );
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("config/existing-dir"),
            &fresh,
            &sha(b""),
        ),
        "file_not_regular",
    );
    for bad in ["", "ABC", &"g".repeat(64), &"A".repeat(64)] {
        refused(
            f.files.rename(&f.owner, &from, &fresh, bad),
            "file_path_invalid",
        );
    }
    assert_eq!(f.get("config/source.txt").unwrap(), b"source");
    assert_eq!(f.get("config/existing.txt").unwrap(), b"target");
    assert!(!f.game.join("missing").exists());
    assert!(!f.game.join(fresh.as_str()).exists());
    assert!(!f.audit_actions().iter().any(|a| a == "files.rename"));
}

#[test]
fn mkdir_and_rename_use_distinct_capabilities_and_both_rename_scopes() {
    let f = Fixture::open();
    let stranger = seed(&f.db, false);
    f.put("config/source.txt", b"source");
    let dir = Fixture::path("config/new-dir");
    let from = Fixture::path("config/source.txt");
    let to = Fixture::path("moved.txt");
    f.grant(&stranger, Capability::FilesCreate, "**");
    f.grant(&stranger, Capability::FilesDelete, "**");
    refused(f.files.mkdir(&stranger, &dir), "forbidden");
    refused(
        f.files.rename(&stranger, &from, &to, &sha(b"source")),
        "forbidden",
    );
    f.grant(&stranger, Capability::FilesMkdir, "config/**");
    f.files.mkdir(&stranger, &dir).unwrap();
    refused(
        f.files.mkdir(&stranger, &Fixture::path("outside")),
        "forbidden",
    );
    f.grant(&stranger, Capability::FilesRename, "config/**");
    refused(
        f.files.rename(&stranger, &from, &to, &sha(b"source")),
        "forbidden",
    );
    refused(
        f.files.rename(&stranger, &to, &from, &sha(b"source")),
        "forbidden",
    );
    #[cfg(any(windows, target_os = "linux"))]
    {
        f.grant(&stranger, Capability::FilesRename, "moved.txt");
        f.files
            .rename(&stranger, &from, &to, &sha(b"source"))
            .unwrap();
    }
}

#[test]
fn mkdir_and_rename_refuse_protected_traversal_and_ambiguous_names() {
    let f = Fixture::open();
    for text in [
        "../escape",
        "config/../escape",
        "/absolute",
        r"config\escape",
        "config/name.",
        "config/name ",
        "config/name:stream",
        "config/CON",
        "config/COM0",
    ] {
        refused(RelativePath::parse(text), "file_path_invalid");
    }
    for text in [
        ".vesta/child",
        "config/.git",
        "WRAPPER-CONFIG.TOML",
        "config/start-vesta-wrapper.bat",
    ] {
        refused(RelativePath::parse(text), "file_path_protected");
    }
    let protected = f.game.join("secrets");
    let files = FileBrowser::open(f.db.clone(), &f.game, &[&protected]).unwrap();
    f.put("config/source.txt", b"source");
    refused(
        files.mkdir(&f.owner, &Fixture::path("secrets")),
        "file_path_protected",
    );
    refused(
        files.rename(
            &f.owner,
            &Fixture::path("config/source.txt"),
            &Fixture::path("secrets"),
            &sha(b"source"),
        ),
        "file_path_protected",
    );
    fs::create_dir(&protected).unwrap();
    f.put("secrets/hidden.txt", b"hidden");
    refused(
        files.rename(
            &f.owner,
            &Fixture::path("secrets/hidden.txt"),
            &Fixture::path("config/moved.txt"),
            &sha(b"hidden"),
        ),
        "file_path_protected",
    );
    assert_eq!(f.get("config/source.txt").unwrap(), b"source");
    assert_eq!(f.get("secrets/hidden.txt").unwrap(), b"hidden");
}

#[test]
fn mkdir_and_rename_refuse_hard_link_aliases() {
    let f = Fixture::open();
    f.put("config/source.txt", b"source");
    fs::hard_link(
        f.game.join("config/source.txt"),
        f.game.join("config/alias.txt"),
    )
    .unwrap();
    let from = Fixture::path("config/source.txt");
    let to = Fixture::path("config/moved.txt");
    refused(
        f.files.rename(&f.owner, &from, &to, &sha(b"source")),
        "file_path_linked",
    );
    refused(f.files.mkdir(&f.owner, &from), "file_path_linked");
    f.put("config/plain.txt", b"plain");
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("config/plain.txt"),
            &from,
            &sha(b"plain"),
        ),
        "file_path_linked",
    );
    assert_eq!(f.get("config/source.txt").unwrap(), b"source");
    assert_eq!(f.get("config/alias.txt").unwrap(), b"source");
    assert!(!f.game.join(to.as_str()).exists());
}

#[cfg(windows)]
#[test]
fn mkdir_and_rename_refuse_junction_ancestors_and_sources() {
    let f = Fixture::open();
    let outside = f._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("source.txt"), b"outside").unwrap();
    let linked = f.game.join("linked");
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&linked)
        .arg(&outside)
        .status()
        .unwrap();
    assert!(status.success());
    refused(
        f.files.mkdir(&f.owner, &Fixture::path("linked/new-dir")),
        "file_path_linked",
    );
    refused(
        f.files.mkdir(&f.owner, &Fixture::path("linked")),
        "file_path_linked",
    );
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("linked/source.txt"),
            &Fixture::path("config/moved.txt"),
            &sha(b"outside"),
        ),
        "file_path_linked",
    );
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("linked"),
            &Fixture::path("config/moved.txt"),
            &sha(b""),
        ),
        "file_path_linked",
    );
    f.put("config/plain.txt", b"plain");
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("config/plain.txt"),
            &Fixture::path("linked/moved.txt"),
            &sha(b"plain"),
        ),
        "file_path_linked",
    );
    assert!(!outside.join("new-dir").exists());
    assert_eq!(fs::read(outside.join("source.txt")).unwrap(), b"outside");
    fs::remove_dir(linked).unwrap();
}

#[cfg(unix)]
#[test]
fn mkdir_and_rename_refuse_symlink_sources_and_parents() {
    use std::os::unix::fs::symlink;
    let f = Fixture::open();
    f.put("config/source.txt", b"source");
    symlink(f.game.join("config/source.txt"), f.game.join("alias.txt")).unwrap();
    symlink(f.game.join("config"), f.game.join("linked")).unwrap();
    refused(
        f.files.mkdir(&f.owner, &Fixture::path("linked/new-dir")),
        "file_path_linked",
    );
    refused(
        f.files.rename(
            &f.owner,
            &Fixture::path("alias.txt"),
            &Fixture::path("config/moved.txt"),
            &sha(b"source"),
        ),
        "file_path_linked",
    );
}

#[cfg(windows)]
#[test]
fn mkdir_and_rename_check_canonical_case_and_protected_short_aliases() {
    let f = Fixture::open();
    fs::create_dir(f.game.join("config/CanonicalLongDirectory")).unwrap();
    f.put("config/CanonicalLongName.toml", b"source");
    f.put("wrapper-config.toml", b"protected");
    let stranger = seed(&f.db, false);
    f.grant(
        &stranger,
        Capability::FilesMkdir,
        "config/CANONICALLONGDIRECTORY/**",
    );
    refused(
        f.files.mkdir(
            &stranger,
            &Fixture::path("config/CANONICALLONGDIRECTORY/new"),
        ),
        "forbidden",
    );
    f.grant(
        &stranger,
        Capability::FilesRename,
        "config/CANONICALLONGNAME.TOML",
    );
    f.grant(&stranger, Capability::FilesRename, "config/new.txt");
    refused(
        f.files.rename(
            &stranger,
            &Fixture::path("config/CANONICALLONGNAME.TOML"),
            &Fixture::path("config/new.txt"),
            &sha(b"source"),
        ),
        "forbidden",
    );
    if let Some(alias) = short_leaf(&f.game.join("wrapper-config.toml")) {
        let path = Fixture::path(&alias);
        refused(f.files.mkdir(&f.owner, &path), "file_path_protected");
        refused(
            f.files.rename(
                &f.owner,
                &path,
                &Fixture::path("config/new.txt"),
                &sha(b"protected"),
            ),
            "file_path_protected",
        );
        refused(
            f.files.rename(
                &f.owner,
                &Fixture::path("config/CanonicalLongName.toml"),
                &path,
                &sha(b"source"),
            ),
            "file_path_protected",
        );
    }
    assert_eq!(f.get("wrapper-config.toml").unwrap(), b"protected");
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn native_rename_never_replaces_a_target_that_appeared_after_preflight() {
    let f = Fixture::open();
    f.put("config/source.txt", b"source");
    let source = f.game.join("config/source.txt");
    let destination = f.game.join("config/new.txt");
    require_absent(&destination).unwrap();
    fs::write(&destination, b"intervening").unwrap();
    refused(
        native::rename_absent(&source, &destination),
        "file_conflict",
    );
    assert_eq!(f.get("config/source.txt").unwrap(), b"source");
    assert_eq!(f.get("config/new.txt").unwrap(), b"intervening");
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn mkdir_and_rename_compensate_failed_audit_and_failed_commit() {
    for commit_failure in [false, true] {
        let f = Fixture::open();
        f.put("config/source.txt", b"source");
        f.db.write(|c| {
            if commit_failure {
                c.execute_batch("CREATE TABLE deferred_file_failure (principal TEXT REFERENCES principals(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_new_file_commit AFTER INSERT ON audit_events WHEN NEW.action IN ('files.mkdir','files.rename') BEGIN INSERT INTO deferred_file_failure VALUES ('missing-principal'); END;")?;
            } else {
                c.execute_batch("CREATE TRIGGER reject_new_file_audit BEFORE INSERT ON audit_events WHEN NEW.action IN ('files.mkdir','files.rename') BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
            }
            Ok(())
        }).unwrap();
        let dir = Fixture::path("config/new-dir");
        let from = Fixture::path("config/source.txt");
        let to = Fixture::path("config/moved.txt");
        assert!(f.files.mkdir(&f.owner, &dir).is_err());
        assert!(!f.game.join(dir.as_str()).exists());
        assert!(
            f.files
                .rename(&f.owner, &from, &to, &sha(b"source"))
                .is_err()
        );
        assert_eq!(f.get(from.as_str()).unwrap(), b"source");
        assert!(!f.game.join(to.as_str()).exists());
        assert!(
            !f.audit_actions()
                .iter()
                .any(|a| a == "files.mkdir" || a == "files.rename")
        );
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn rename_compensation_retains_unknown_effects_and_new_source_data() {
    let f = Fixture::open();
    let source = f.game.join("config/source.txt");
    let destination = f.game.join("config/moved.txt");
    fs::write(&destination, b"foreign").unwrap();
    refused(
        settle_rename::<()>(
            Err(FileError::Unavailable),
            &source,
            &destination,
            &sha(b"expected"),
        ),
        "file_recovery_required",
    );
    assert_eq!(fs::read(&destination).unwrap(), b"foreign");
    fs::write(&destination, b"expected").unwrap();
    fs::write(&source, b"new source").unwrap();
    refused(
        settle_rename::<()>(
            Err(FileError::Unavailable),
            &source,
            &destination,
            &sha(b"expected"),
        ),
        "file_recovery_required",
    );
    assert_eq!(fs::read(&source).unwrap(), b"new source");
    assert_eq!(fs::read(&destination).unwrap(), b"expected");
}

fn reopen(f: &mut Fixture) {
    let directory = f.db.directory().clone();
    f.db.close().unwrap();
    f.db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    f.files = FileBrowser::open(f.db.clone(), &f.game, &[]).unwrap();
}

#[test]
fn every_native_effect_has_durable_intent_and_an_audited_crash_outcome() {
    for kind in [
        "create",
        "overwrite",
        "delete",
        "mkdir",
        "rename",
        "restore",
    ] {
        for point in ["before", "after"] {
            let mut f = Fixture::open();
            let path = Fixture::path("config/target.txt");
            let to = Fixture::path("config/destination.txt");
            if matches!(kind, "overwrite" | "delete" | "rename" | "restore") {
                f.put(path.as_str(), b"old");
            }
            let restore = if kind == "restore" {
                Some(f.files.delete(&f.owner, &path, &sha(b"old")).unwrap().id)
            } else {
                None
            };
            journal::interrupt_at(point);
            let result = match kind {
                "create" => f
                    .files
                    .write(&f.owner, &path, &WritePrecondition::Absent, b"new")
                    .map(|_| ()),
                "overwrite" => f
                    .files
                    .write(&f.owner, &path, &matches(b"old"), b"new")
                    .map(|_| ()),
                "delete" => f.files.delete(&f.owner, &path, &sha(b"old")).map(|_| ()),
                "mkdir" => f.files.mkdir(&f.owner, &path),
                "rename" => f
                    .files
                    .rename(&f.owner, &path, &to, &sha(b"old"))
                    .map(|_| ()),
                "restore" => f
                    .files
                    .restore(&f.owner, restore.as_deref().unwrap())
                    .map(|_| ()),
                _ => unreachable!(),
            };
            refused(result, "file_recovery_required");
            let pending: i64 =
                f.db.read(|c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM file_intents WHERE status='pending'",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(pending, 1, "{kind} {point}");
            if point == "before" {
                assert_eq!(
                    f.get(path.as_str()),
                    if matches!(kind, "overwrite" | "delete" | "rename") {
                        Some(b"old".to_vec())
                    } else {
                        None
                    }
                );
                assert!(!f.game.join(to.as_str()).exists());
                if matches!(kind, "create" | "overwrite" | "restore") {
                    assert_eq!(f.stages("config"), 1);
                }
            } else {
                match kind {
                    "create" | "overwrite" => assert_eq!(f.get(path.as_str()).unwrap(), b"new"),
                    "delete" => assert!(f.get(path.as_str()).is_none()),
                    "mkdir" => assert!(f.game.join(path.as_str()).is_dir()),
                    "rename" => {
                        assert!(f.get(path.as_str()).is_none());
                        assert_eq!(f.get(to.as_str()).unwrap(), b"old");
                    }
                    "restore" => assert_eq!(f.get(path.as_str()).unwrap(), b"old"),
                    _ => unreachable!(),
                }
            }
            reopen(&mut f);
            assert_eq!(f.files.reconcile_pending().unwrap(), 1, "{kind} {point}");
            assert_eq!(f.files.reconcile_pending().unwrap(), 0);
            let (outcome, metadata, intent_id): (String, String, String) = f.db.read(|c| Ok(c.query_row("SELECT outcome,metadata_json,operation_id FROM audit_events WHERE source='files' AND json_extract(metadata_json,'$.reconciled')='true' ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)).unwrap();
            let status: Option<String> =
                f.db.read(|c| {
                    Ok(c.query_row(
                        "SELECT status FROM file_intents WHERE id=?",
                        [&intent_id],
                        |r| r.get(0),
                    )
                    .optional()?)
                })
                .unwrap();
            let expected = if point == "before" {
                "aborted"
            } else if kind == "mkdir" {
                "recovery-required"
            } else {
                "complete"
            };
            assert_eq!(
                status.as_deref(),
                (expected == "recovery-required").then_some(expected),
                "{kind} {point}"
            );
            assert_eq!(
                outcome,
                if expected == "complete" {
                    "success"
                } else {
                    expected
                }
            );
            assert!(metadata.contains("\"reconciled\":\"true\""));
            assert_eq!(f.stages("config"), 0);
            assert!(
                AuditLog::new(f.db.clone())
                    .unwrap()
                    .verify_chain()
                    .unwrap()
                    .valid
            );
        }
    }
}

#[test]
fn recovery_never_overwrites_an_unknown_intervening_target() {
    for kind in ["create", "overwrite", "delete", "rename"] {
        let mut f = Fixture::open();
        let path = Fixture::path("config/target.txt");
        let to = Fixture::path("config/moved.txt");
        if kind != "create" {
            f.put(path.as_str(), b"old");
        }
        journal::interrupt_at("after");
        let result = match kind {
            "create" => f
                .files
                .write(&f.owner, &path, &WritePrecondition::Absent, b"new")
                .map(|_| ()),
            "overwrite" => f
                .files
                .write(&f.owner, &path, &matches(b"old"), b"new")
                .map(|_| ()),
            "delete" => f.files.delete(&f.owner, &path, &sha(b"old")).map(|_| ()),
            _ => f
                .files
                .rename(&f.owner, &path, &to, &sha(b"old"))
                .map(|_| ()),
        };
        assert!(result.is_err());
        let foreign = if kind == "rename" {
            to.as_str()
        } else {
            path.as_str()
        };
        f.put(foreign, b"foreign edit");
        reopen(&mut f);
        assert_eq!(f.files.reconcile_pending().unwrap(), 1);
        assert_eq!(f.get(foreign).unwrap(), b"foreign edit");
        assert_eq!(
            f.db.read(|c| Ok(
                c.query_row("SELECT status FROM file_intents LIMIT 1", [], |r| r
                    .get::<_, String>(0))?
            ))
            .unwrap(),
            "recovery-required"
        );
    }
}

#[test]
fn nested_audit_paths_below_at_and_above_boundary_are_bounded_and_identified() {
    let f = Fixture::open();
    let parent_text = format!(
        "config/{}/{}/{}",
        "a".repeat(80),
        "b".repeat(80),
        "c".repeat(70)
    );
    fs::create_dir_all(f.game.join(&parent_text)).unwrap();
    for units in [255, 256, 257, 400] {
        let text = format!(
            "{parent_text}/{}",
            "x".repeat(units - parent_text.len() - 1)
        );
        let path = Fixture::path(&text);
        f.files
            .write(&f.owner, &path, &WritePrecondition::Absent, b"new")
            .unwrap();
        let audit = AuditLog::new(f.db.clone())
            .unwrap()
            .page(0, 200)
            .unwrap()
            .pop()
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&audit.metadata_json).unwrap();
        assert_eq!(json["path_sha256"], sha(text.as_bytes()));
        assert!(json["path"].as_str().unwrap().encode_utf16().count() <= 256);
        if units > 256 {
            assert!(json["path"].as_str().unwrap().contains("[abbrev]"));
        } else {
            assert_eq!(json["path"], text);
        }
        assert_eq!(f.get(&text).unwrap(), b"new");
    }
    f.put("config/source.txt", b"source");
    let to = Fixture::path(&format!("{parent_text}/{}", "d".repeat(180)));
    f.files
        .rename(
            &f.owner,
            &Fixture::path("config/source.txt"),
            &to,
            &sha(b"source"),
        )
        .unwrap();
    let audit = AuditLog::new(f.db.clone())
        .unwrap()
        .page(0, 200)
        .unwrap()
        .pop()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&audit.metadata_json).unwrap();
    assert_eq!(json["to_sha256"], sha(to.as_str().as_bytes()));
    assert!(json["to"].as_str().unwrap().encode_utf16().count() <= 256);
    assert_eq!(f.get(to.as_str()).unwrap(), b"source");
}

#[test]
fn supplementary_unicode_path_display_preserves_full_identity() {
    for units in [254, 256, 258, 4096] {
        let text = "\u{1f680}".repeat(units / 2);
        let fields = path_fields("path", &text);
        assert!(fields[0].1.as_ref().unwrap().encode_utf16().count() <= 256);
        assert_eq!(fields[1].1.as_deref(), Some(sha(text.as_bytes()).as_str()));
    }
}

#[test]
fn failed_intent_admission_refuses_every_effect_and_cleans_owned_staging() {
    for kind in [
        "create",
        "overwrite",
        "delete",
        "mkdir",
        "rename",
        "restore",
    ] {
        let f = Fixture::open();
        let path = Fixture::path("config/target.txt");
        if matches!(kind, "overwrite" | "delete" | "rename" | "restore") {
            f.put(path.as_str(), b"old");
        }
        let restore = if kind == "restore" {
            Some(f.files.delete(&f.owner, &path, &sha(b"old")).unwrap().id)
        } else {
            None
        };
        f.db.write(|c| { c.execute_batch("CREATE TRIGGER reject_intent BEFORE INSERT ON file_intents BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
        let result = match kind {
            "create" => f
                .files
                .write(&f.owner, &path, &WritePrecondition::Absent, b"new")
                .map(|_| ()),
            "overwrite" => f
                .files
                .write(&f.owner, &path, &matches(b"old"), b"new")
                .map(|_| ()),
            "delete" => f.files.delete(&f.owner, &path, &sha(b"old")).map(|_| ()),
            "mkdir" => f.files.mkdir(&f.owner, &path),
            "rename" => f
                .files
                .rename(
                    &f.owner,
                    &path,
                    &Fixture::path("config/to.txt"),
                    &sha(b"old"),
                )
                .map(|_| ()),
            _ => f
                .files
                .restore(&f.owner, restore.as_deref().unwrap())
                .map(|_| ()),
        };
        assert!(result.is_err(), "{kind}");
        assert_eq!(
            f.get(path.as_str()),
            if matches!(kind, "overwrite" | "delete" | "rename") {
                Some(b"old".to_vec())
            } else {
                None
            }
        );
        assert!(!f.game.join("config/to.txt").exists());
        assert!(!f.game.join(path.as_str()).is_dir());
        assert_eq!(f.stages("config"), 0);
        assert_eq!(f.trash_rows(), i64::from(kind == "restore"));
    }
}

#[test]
fn failed_recovered_audit_preserves_pending_intent_and_owned_stage_for_retry() {
    let mut f = Fixture::open();
    let path = Fixture::path("config/target.txt");
    journal::interrupt_at("before");
    assert!(
        f.files
            .write(&f.owner, &path, &WritePrecondition::Absent, b"new")
            .is_err()
    );
    reopen(&mut f);
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER reject_recovered_audit BEFORE INSERT ON audit_events WHEN NEW.source='files' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(f.files.reconcile_pending().is_err());
    assert_eq!(f.stages("config"), 1);
    assert_eq!(
        f.db.read(
            |c| Ok(c.query_row("SELECT status FROM file_intents", [], |r| r
                .get::<_, String>(0))?)
        )
        .unwrap(),
        "pending"
    );
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER reject_recovered_audit")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(f.files.reconcile_pending().unwrap(), 1);
    assert_eq!(f.stages("config"), 0);
    assert!(f.get(path.as_str()).is_none());
}

#[test]
fn equal_hash_overwrites_abort_only_when_an_unconsumed_stage_proves_no_effect() {
    for point in ["before", "after"] {
        let mut f = Fixture::open();
        let path = Fixture::path("config/target.txt");
        f.put(path.as_str(), b"same");
        journal::interrupt_at(point);
        assert!(
            f.files
                .write(&f.owner, &path, &matches(b"same"), b"same")
                .is_err()
        );
        reopen(&mut f);
        assert_eq!(f.files.reconcile_pending().unwrap(), 1);
        assert_eq!(
            f.db.read(|c| Ok(c
                .query_row("SELECT status FROM file_intents", [], |r| r
                    .get::<_, String>(0))
                .optional()?))
                .unwrap(),
            if point == "before" {
                None
            } else {
                Some("recovery-required".to_owned())
            }
        );
        assert_eq!(f.get(path.as_str()).unwrap(), b"same");
        assert_eq!(f.trash_rows(), 1);
    }
}

#[test]
fn changed_pending_stage_is_retained_and_has_an_explicit_recovery_audit() {
    let mut f = Fixture::open();
    journal::interrupt_at("before");
    assert!(
        f.files
            .write(
                &f.owner,
                &Fixture::path("config/target.txt"),
                &WritePrecondition::Absent,
                b"new"
            )
            .is_err()
    );
    let stage = fs::read_dir(f.game.join("config"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(STAGE_PREFIX)
        })
        .unwrap();
    fs::write(&stage, b"foreign stage content").unwrap();
    reopen(&mut f);
    assert_eq!(f.files.reconcile_pending().unwrap(), 1);
    assert_eq!(fs::read(&stage).unwrap(), b"foreign stage content");
    assert_eq!(
        f.db.read(
            |c| Ok(c.query_row("SELECT status FROM file_intents", [], |r| r
                .get::<_, String>(0))?)
        )
        .unwrap(),
        "recovery-required"
    );
    assert_eq!(
        AuditLog::new(f.db.clone())
            .unwrap()
            .page(0, 200)
            .unwrap()
            .last()
            .unwrap()
            .outcome,
        "recovery-required"
    );
}

#[test]
fn pending_upload_staging_survives_expiry_and_recovery_retains_uncertain_bytes() {
    for (point, foreign) in [("before", false), ("after", false), ("after", true)] {
        let mut f = Fixture::open();
        let path = Fixture::path("config/target.txt");
        let upload = f
            .files
            .begin(&f.owner, &path, &WritePrecondition::Absent, 3, &sha(b"new"))
            .unwrap();
        f.files.append(&f.owner, &upload, 0, b"new").unwrap();
        journal::interrupt_at(point);
        assert!(f.files.finish(&f.owner, &upload).is_err());
        refused(f.files.abort(&f.owner, &upload), "file_recovery_required");
        f.clock
            .0
            .store(START + 8 * 24 * 60 * 60 * 1000, Ordering::SeqCst);
        assert_eq!(f.files.purge_expired_uploads().unwrap(), 0);
        if foreign {
            f.put(path.as_str(), b"foreign");
        }
        reopen(&mut f);
        assert_eq!(f.files.reconcile_pending().unwrap(), 1);
        assert_eq!(
            f.files.purge_expired_uploads().unwrap(),
            usize::from(!foreign)
        );
        let upload_copy =
            f.db.directory()
                .path()
                .join("uploads")
                .join(format!("{}.bin", upload.as_str()));
        assert_eq!(upload_copy.exists(), foreign);
        if foreign {
            assert_eq!(fs::read(upload_copy).unwrap(), b"new");
        }
    }
}

#[test]
fn pending_restore_keeps_its_original_copy_even_when_trash_expired() {
    let mut f = Fixture::open();
    let path = Fixture::path("config/target.txt");
    f.put(path.as_str(), b"old");
    let trash = f.files.delete(&f.owner, &path, &sha(b"old")).unwrap();
    journal::interrupt_at("before");
    assert!(f.files.restore(&f.owner, &trash.id).is_err());
    f.clock.0.store(trash.expires_at, Ordering::SeqCst);
    assert_eq!(f.files.purge_expired_trash().unwrap(), 0);
    assert_eq!(f.trash_files(), 1);
    assert_eq!(f.stages("config"), 1);
    reopen(&mut f);
    assert_eq!(f.files.reconcile_pending().unwrap(), 1);
    assert_eq!(f.stages("config"), 0);
    assert!(f.get(path.as_str()).is_none());
}

#[path = "intent_tests.rs"]
mod intent_tests;

#[test]
fn remote_outcome_commit_failure_compensates_and_exact_retry_survives_reopen() {
    use crate::files::RemoteFileMutation;
    let mut f = Fixture::open();
    let path = Fixture::path("config/remote.txt");
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER fail_remote_outcome BEFORE UPDATE OF outcome_json ON management_file_outcomes BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(
        f.files
            .mutate_remote(
                &f.owner,
                "remote-file-key-0001",
                RemoteFileMutation::Write {
                    path: &path,
                    precondition: &WritePrecondition::Absent,
                    bytes: b"remote"
                }
            )
            .is_err()
    );
    assert!(f.get(path.as_str()).is_none());
    assert_eq!(f.stages("config"), 0);
    let completed: i64 =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM management_file_outcomes WHERE outcome_json IS NOT NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(completed, 0);
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER fail_remote_outcome")?;
        Ok(())
    })
    .unwrap();
    let first = f
        .files
        .mutate_remote(
            &f.owner,
            "remote-file-key-0001",
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Absent,
                bytes: b"remote",
            },
        )
        .unwrap();
    reopen(&mut f);
    assert_eq!(
        first,
        f.files
            .mutate_remote(
                &f.owner,
                "remote-file-key-0001",
                RemoteFileMutation::Write {
                    path: &path,
                    precondition: &WritePrecondition::Absent,
                    bytes: b"remote"
                }
            )
            .unwrap()
    );
    let successes:i64=f.db.read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'",[],|r|r.get(0))?)).unwrap();
    assert_eq!(successes, 1);
    assert!(
        AuditLog::new(f.db.clone())
            .unwrap()
            .verify_chain()
            .unwrap()
            .valid
    );
}
#[test]
fn remote_pending_request_after_interruption_never_blindly_replays() {
    use crate::files::RemoteFileMutation;
    let mut f = Fixture::open();
    let path = Fixture::path("config/interrupted.txt");
    journal::interrupt_at("after");
    assert!(matches!(
        f.files.mutate_remote(
            &f.owner,
            "remote-file-key-0002",
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Absent,
                bytes: b"retained"
            }
        ),
        Err(FileError::RecoveryRequired(_))
    ));
    reopen(&mut f);
    f.files.reconcile_pending().unwrap();
    assert!(matches!(
        f.files.mutate_remote(
            &f.owner,
            "remote-file-key-0002",
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Absent,
                bytes: b"retained"
            }
        ),
        Err(FileError::RecoveryRequired(_))
    ));
    assert_eq!(f.get(path.as_str()).unwrap(), b"retained");
}

#[test]
fn remote_trash_is_principal_and_device_owned_even_with_full_file_grants() {
    use crate::files::RemoteFileMutation;
    let f = Fixture::open();
    let path = Fixture::path("config/owned.txt");
    f.put(path.as_str(), b"owned");
    let first = f
        .files
        .mutate_remote(
            &f.owner,
            "owned-trash-delete-01",
            RemoteFileMutation::Delete {
                path: &path,
                expected_sha256: &sha(b"owned"),
            },
        )
        .unwrap();
    let id = first["trash_id"].as_str().unwrap();
    let remote = f.files.for_remote();
    assert_eq!(remote.trash_entries(&f.owner).unwrap().len(), 1);
    for different in ["principal", "device"] {
        let other = if different == "principal" {
            seed(&f.db, false)
        } else {
            let device =
                crate::identity::DeviceId::parse("cccccccc-2222-4333-8444-555555555555").unwrap();
            f.db.write(|c| { c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Other device',?,?)",params![device.as_str(),f.db.server_id(),f.owner.principal.as_str(),"0".repeat(64),f.db.clock().millis()])?; Ok(()) }).unwrap();
            Subject {
                principal: f.owner.principal.clone(),
                device,
            }
        };
        for cap in [Capability::FilesRead, Capability::FilesCreate] {
            f.grant(&other, cap, "*");
        }
        assert!(remote.trash_entries(&other).unwrap().is_empty());
        assert!(matches!(
            remote.mutate_remote(
                &other,
                "owned-trash-restore1",
                RemoteFileMutation::Restore { id }
            ),
            Err(FileError::NotFound)
        ));
        assert!(f.get(path.as_str()).is_none());
    }
    remote
        .mutate_remote(
            &f.owner,
            "owned-trash-restore1",
            RemoteFileMutation::Restore { id },
        )
        .unwrap();
    assert_eq!(f.get(path.as_str()).unwrap(), b"owned");
}
#[test]
fn remote_outcome_retention_is_bounded_and_never_purges_pending_rows() {
    use crate::files::RemoteFileMutation;
    let f = Fixture::open();
    let now = f.db.clock().millis();
    f.db.write(|c| {
        let sql = "INSERT INTO management_file_outcomes VALUES (?,?,?,?,?,?,?,?)";
        for i in 0..130 {
            c.execute(
                sql,
                params![
                    f.db.server_id(),
                    f.owner.principal.as_str(),
                    f.owner.device.as_str(),
                    format!("expired-{i}"),
                    "a".repeat(64),
                    "[]",
                    "{}",
                    now - 8 * 24 * 60 * 60 * 1000
                ],
            )?;
        }
        c.execute(
            sql,
            params![
                f.db.server_id(),
                f.owner.principal.as_str(),
                f.owner.device.as_str(),
                "pending-retained",
                "a".repeat(64),
                "[]",
                Option::<String>::None,
                Option::<i64>::None
            ],
        )?;
        c.execute(
            sql,
            params![
                f.db.server_id(),
                f.owner.principal.as_str(),
                f.owner.device.as_str(),
                "recent-retained",
                "a".repeat(64),
                "[]",
                "{}",
                now
            ],
        )?;
        Ok(())
    })
    .unwrap();
    let counts = || {
        f.db.read(|c| Ok(c.query_row("SELECT count(*),sum(request_key LIKE 'expired-%'),sum(request_key='pending-retained'),sum(request_key='recent-retained') FROM management_file_outcomes",[],|r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?)).unwrap()
    };
    let path = Fixture::path("config/retained.txt");
    f.files
        .mutate_remote(
            &f.owner,
            "retention-write-key1",
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Absent,
                bytes: b"ok",
            },
        )
        .unwrap();
    assert_eq!(counts(), (5, 2, 1, 1));
    f.files
        .mutate_remote(
            &f.owner,
            "retention-write-key1",
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Absent,
                bytes: b"ok",
            },
        )
        .unwrap();
    assert_eq!(counts(), (3, 0, 1, 1));
}

#[test]
fn remote_canonical_admission_documents_refuse_all_effects_and_uploads() {
    use crate::files::RemoteFileMutation;
    let f = Fixture::open();
    f.put("config/source.txt", b"source");
    let remote = f.files.for_remote();
    for name in [
        "server.properties",
        "SERVER.PROPERTIES",
        "whitelist.json",
        "OPS.JSON",
        "banned-players.json",
        "banned-ips.json",
    ] {
        f.put(name, b"secret");
        let path = Fixture::path(name);
        let to = Fixture::path("config/destination.txt");
        let source = Fixture::path("config/source.txt");
        assert!(matches!(
            remote.read(&f.owner, &path, 1024),
            Err(FileError::ProtectedPath)
        ));
        for request in [
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Matches(sha(b"secret")),
                bytes: b"unsafe",
            },
            RemoteFileMutation::Delete {
                path: &path,
                expected_sha256: &sha(b"secret"),
            },
            RemoteFileMutation::Mkdir { path: &path },
            RemoteFileMutation::Rename {
                from: &path,
                to: &to,
                expected_sha256: &sha(b"secret"),
            },
            RemoteFileMutation::Rename {
                from: &source,
                to: &path,
                expected_sha256: &sha(b"source"),
            },
            RemoteFileMutation::Begin {
                path: &path,
                precondition: &WritePrecondition::Matches(sha(b"secret")),
                total_size: 0,
                sha256: &sha(b""),
            },
        ] {
            assert!(matches!(
                remote.mutate_remote(&f.owner, "canonical-refusal-key", request),
                Err(FileError::ProtectedPath)
            ));
        }
        assert_eq!(f.get(name).unwrap(), b"secret");
    }
}
#[cfg(windows)]
#[test]
fn real_short_admission_aliases_refuse_remote_reads_mutations_uploads_and_restore() {
    use crate::files::RemoteFileMutation;
    let f = Fixture::open();
    f.put("config/source.txt", b"source");
    let remote = f.files.for_remote();
    for name in [
        "server.properties",
        "whitelist.json",
        "ops.json",
        "banned-players.json",
        "banned-ips.json",
    ] {
        f.put(name, b"secret");
        let Some(alias) = short_leaf(&f.game.join(name)) else {
            continue;
        };
        let path = Fixture::path(&alias);
        let source = Fixture::path("config/source.txt");
        let to = Fixture::path("config/destination.txt");
        assert!(matches!(
            remote.read(&f.owner, &path, 1024),
            Err(FileError::ProtectedPath)
        ));
        for request in [
            RemoteFileMutation::Write {
                path: &path,
                precondition: &WritePrecondition::Matches(sha(b"secret")),
                bytes: b"unsafe",
            },
            RemoteFileMutation::Delete {
                path: &path,
                expected_sha256: &sha(b"secret"),
            },
            RemoteFileMutation::Mkdir { path: &path },
            RemoteFileMutation::Rename {
                from: &path,
                to: &to,
                expected_sha256: &sha(b"secret"),
            },
            RemoteFileMutation::Rename {
                from: &source,
                to: &path,
                expected_sha256: &sha(b"source"),
            },
            RemoteFileMutation::Begin {
                path: &path,
                precondition: &WritePrecondition::Matches(sha(b"secret")),
                total_size: 0,
                sha256: &sha(b""),
            },
        ] {
            assert!(matches!(
                remote.mutate_remote(&f.owner, "short-alias-refuse-01", request),
                Err(FileError::ProtectedPath)
            ));
        }
        let staged = f
            .files
            .begin(
                &f.owner,
                &path,
                &WritePrecondition::Matches(sha(b"secret")),
                0,
                &sha(b""),
            )
            .unwrap();
        assert!(matches!(
            remote.mutate_remote(
                &f.owner,
                "short-alias-finish-01",
                RemoteFileMutation::Finish { id: &staged }
            ),
            Err(FileError::ProtectedPath)
        ));
        f.files.abort(&f.owner, &staged).unwrap();
        let entry = f.files.delete(&f.owner, &path, &sha(b"secret")).unwrap();
        assert_eq!(entry.path.as_str(), name);
        assert!(matches!(
            remote.mutate_remote(
                &f.owner,
                "short-alias-restore1",
                RemoteFileMutation::Restore { id: &entry.id }
            ),
            Err(FileError::NotFound | FileError::ProtectedPath)
        ));
        assert!(f.get(name).is_none());
    }
}

#[test]
fn stalled_native_mutation_expires_install_releases_global_and_allows_exact_retry() {
    use std::time::{Duration, Instant};
    let f = Fixture::open();
    let admission = mutation_admission(f.db.directory().path()).unwrap();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = admission.lock().unwrap();
        entered.send(()).unwrap();
        let _ = blocked.recv_timeout(Duration::from_secs(5));
    });
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    let path = RelativePath::parse("deadline.jar").unwrap();
    let start = Instant::now();
    let installed = f
        .files
        .for_content(start + Duration::from_millis(100))
        .execute_remote(
            &f.owner,
            "native-deadline-install",
            serde_json::json!(["install-fixture"]),
            || {
                Ok(vec![(
                    Capability::FilesCreate.id().into(),
                    path.as_str().into(),
                )])
            },
            |files| {
                let _global = files.remote_effect_admission()?;
                let change =
                    files.write(&f.owner, &path, &WritePrecondition::Absent, b"verified")?;
                Ok(serde_json::json!({"sha256":change.sha256}))
            },
        );
    assert!(matches!(installed, Err(FileError::Unavailable)));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "native wait exceeded install deadline"
    );
    assert!(!f.game.join(path.as_str()).exists());
    f.db.read(|c| {
        let outcomes: i64 =
            c.query_row("SELECT count(*) FROM management_file_outcomes", [], |r| {
                r.get(0)
            })?;
        let intents: i64 = c.query_row("SELECT count(*) FROM file_intents", [], |r| r.get(0))?;
        assert_eq!((outcomes, intents), (0, 0));
        Ok(())
    })
    .unwrap();
    // The other database has an independent native slot. Check global admission
    // while the competing native lock above is still held.
    let other = Fixture::open();
    let start = Instant::now();
    other
        .files
        .mutate_remote(
            &other.owner,
            "unrelated-after-expiry",
            crate::files::RemoteFileMutation::Write {
                path: &RelativePath::parse("unrelated.txt").unwrap(),
                precondition: &WritePrecondition::Absent,
                bytes: b"unrelated",
            },
        )
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    holder.join().unwrap();
    f.files
        .for_content(Instant::now() + Duration::from_secs(1))
        .execute_remote(
            &f.owner,
            "native-deadline-install",
            serde_json::json!(["install-fixture"]),
            || {
                Ok(vec![(
                    Capability::FilesCreate.id().into(),
                    path.as_str().into(),
                )])
            },
            |files| {
                let _global = files.remote_effect_admission()?;
                let change =
                    files.write(&f.owner, &path, &WritePrecondition::Absent, b"verified")?;
                Ok(serde_json::json!({"sha256":change.sha256}))
            },
        )
        .unwrap();
    assert_eq!(fs::read(f.game.join(path.as_str())).unwrap(), b"verified");
}

#[test]
fn ordinary_remote_native_admission_shares_the_request_deadline() {
    use std::time::{Duration, Instant};
    let f = Fixture::open();
    let admission = mutation_admission(f.db.directory().path()).unwrap();
    let _guard = admission.lock().unwrap();
    let mut files = f.files.for_remote();
    files.remote_deadline = Some(Instant::now() + Duration::from_millis(25));
    assert!(matches!(
        files.mutate_remote(
            &f.owner,
            "ordinary-native-deadline",
            crate::files::RemoteFileMutation::Mkdir {
                path: &RelativePath::parse("no-effect").unwrap(),
            }
        ),
        Err(FileError::Unavailable)
    ));
    assert!(!f.game.join("no-effect").exists());
    f.db.read(|c| {
        let count: i64 = c.query_row("SELECT count(*) FROM management_file_outcomes", [], |r| {
            r.get(0)
        })?;
        assert_eq!(count, 0);
        Ok(())
    })
    .unwrap();
}

#[test]
fn offline_whitelist_lost_post_write_proof_never_replays_a_success_receipt() {
    let f = Fixture::open();
    let properties = f.game.join("server.properties");
    fs::write(&properties, b"online-mode=false\n").unwrap();
    let launch = vesta_game_runtime::process::LaunchSpec::new(
        std::env::current_exe().unwrap(),
        vec![],
        f.game.clone(),
        vesta_game_runtime::process::GracefulStop::MinecraftCommand,
    )
    .unwrap();
    let mut runtime = crate::operations::OperationRuntime::open(
        crate::operations::Operations::new(f.db.clone()).unwrap(),
        launch,
    )
    .unwrap();
    let handle = runtime.network_handle();
    let patch = crate::files::OfflineWhitelistPatch {
        player: "Alex".into(),
        add: true,
        expected_sha256: None,
    };
    journal::after_write_for_test(move || {
        fs::write(properties, b"online-mode=true\n").unwrap();
    });
    assert!(matches!(
        f.files
            .patch_offline_whitelist(&handle, &f.owner, "lost-proof", patch.clone()),
        Err(FileError::RecoveryRequired(_))
    ));
    let outcome: Option<String> =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT outcome_json FROM management_file_outcomes WHERE request_key='lost-proof'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(
        outcome.is_none(),
        "uncertain effect must not commit a succeeded receipt"
    );
    assert!(
        f.get("whitelist.json").is_some(),
        "lost proof must preserve the uncertain effect, not compensate without exclusion"
    );
    let pending: i64 =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM file_intents WHERE status='pending'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(pending, 1);
    assert!(matches!(
        f.files
            .patch_offline_whitelist(&handle, &f.owner, "lost-proof", patch),
        Err(FileError::RecoveryRequired(_))
    ));
    runtime.shutdown(std::time::Duration::from_secs(2)).unwrap();
}

#[test]
fn preparation_replay_waits_for_an_active_request_before_claiming_recovery() {
    let f = Fixture::open();
    let files = f.files.clone();
    let subject = f.owner.clone();
    let (started, observed) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let active = std::thread::spawn(move || {
        files
            .execute_remote(
                &subject,
                "active-preparation",
                serde_json::json!(["fixture"]),
                || Ok(vec![]),
                |files| {
                    started.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(10)).unwrap();
                    let outcome = serde_json::json!({"outcome":"succeeded"});
                    files
                        .db
                        .write_with(|tx| {
                            files.remote_complete(tx.connection(), &subject, outcome.clone())
                        })
                        .map_err(FileError::from)?;
                    Ok(outcome)
                },
            )
            .unwrap()
    });
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        release.send(()).unwrap();
    });
    let result = f
        .files
        .remote_preparation()
        .remote_replay_before_preparation(
            &f.owner,
            "active-preparation",
            &serde_json::json!(["fixture"]),
        )
        .unwrap();
    assert_eq!(result.unwrap()["outcome"], "succeeded");
    release.join().unwrap();
    active.join().unwrap();
}

#[test]
fn offline_whitelist_compensation_rechecks_exclusion_at_its_effect_boundary() {
    for replacing in [false, true] {
        let f = Fixture::open();
        let properties = f.game.join("server.properties");
        fs::write(&properties, b"online-mode=false\n").unwrap();
        if replacing {
            f.put("whitelist.json", b"[]");
        }
        let expected = f.files.offline_whitelist(&f.owner).unwrap()["sha256"]
            .as_str()
            .map(str::to_owned);
        let launch = vesta_game_runtime::process::LaunchSpec::new(
            std::env::current_exe().unwrap(),
            vec![],
            f.game.clone(),
            vesta_game_runtime::process::GracefulStop::MinecraftCommand,
        )
        .unwrap();
        let mut runtime = crate::operations::OperationRuntime::open(
            crate::operations::Operations::new(f.db.clone()).unwrap(),
            launch,
        )
        .unwrap();
        f.db.write(|c| { c.execute_batch("CREATE TRIGGER reject_file_audit BEFORE INSERT ON audit_events WHEN NEW.action='files.write' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
        journal::before_compensation_for_test(move || {
            fs::write(properties, b"online-mode=true\n").unwrap();
        });
        let patch = crate::files::OfflineWhitelistPatch {
            player: "Alex".into(),
            add: true,
            expected_sha256: expected,
        };
        assert!(matches!(
            f.files.patch_offline_whitelist(
                &runtime.network_handle(),
                &f.owner,
                "compensation-proof",
                patch
            ),
            Err(FileError::RecoveryRequired(_))
        ));
        let current: serde_json::Value = serde_json::from_slice(
            &f.get("whitelist.json")
                .expect("uncertain file effect retained"),
        )
        .unwrap();
        assert_eq!(
            current[0]["name"], "Alex",
            "compensation must not alter the file without exclusion"
        );
        let status: String =
            f.db.read(|c| Ok(c.query_row("SELECT status FROM file_intents", [], |r| r.get(0))?))
                .unwrap();
        assert_eq!(status, "recovery-required");
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }
}

#[test]
fn external_preparation_and_admission_share_the_original_mutation_deadline() {
    let f = Fixture::open();
    let prepared = f.files.remote_preparation();
    let deadline = prepared.remote_deadline.unwrap();
    assert_eq!(
        prepared.remote_preparation().remote_deadline,
        Some(deadline)
    );
    let mut expired = prepared;
    expired.remote_deadline = Some(std::time::Instant::now() - Duration::from_secs(1));
    let result = expired.execute_remote(
        &f.owner,
        "expired-preparation",
        serde_json::json!(["fixture"]),
        || Ok(vec![]),
        |_| panic!("expired intent must not dispatch"),
    );
    assert!(matches!(result, Err(FileError::Unavailable)));
}
