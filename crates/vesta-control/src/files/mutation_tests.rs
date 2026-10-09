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
