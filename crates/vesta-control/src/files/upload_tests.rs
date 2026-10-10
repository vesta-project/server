use super::*;
use crate::identity::{DeviceId, PrincipalId};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
use tempfile::TempDir;
use vesta_state::{Clock, StateDatabase, StateDirectory, audit::AuditLog};

const START: i64 = 1_791_115_200_000;
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    _temp: TempDir,
    game: PathBuf,
    db: StateDatabase,
    clock: Arc<TestClock>,
    owner: Subject,
    files: FileBrowser,
}
impl Fixture {
    fn open() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(game.join("config")).unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(START)));
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap(),
            clock.clone(),
        )
        .unwrap();
        let owner = seed(&db);
        let files = FileBrowser::open(db.clone(), &game, &[]).unwrap();
        Self {
            _temp: temp,
            game,
            db,
            clock,
            owner,
            files,
        }
    }
    fn begin(&self, bytes: &[u8]) -> UploadId {
        self.files
            .begin(
                &self.owner,
                &path(),
                &WritePrecondition::Absent,
                bytes.len() as u64,
                &sha(bytes),
            )
            .unwrap()
    }
    fn rows(&self) -> i64 {
        self.db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM file_uploads", [], |r| r.get(0))?))
            .unwrap()
    }
    fn stages(&self) -> usize {
        self.db
            .directory()
            .uploads()
            .unwrap()
            .count(100)
            .unwrap()
            .unwrap()
    }
}
fn path() -> RelativePath {
    RelativePath::parse("config/upload.toml").unwrap()
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn seed(db: &StateDatabase) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)", params![principal.as_str(), db.server_id(), START, START])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)", params![device.as_str(), db.server_id(), principal.as_str(), sha(device.as_str().as_bytes()), START])?;
        c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)", params![uuid::Uuid::new_v4().to_string(), principal.as_str(), START])?;
        Ok(())
    }).unwrap();
    Subject { principal, device }
}
fn code<T: fmt::Debug>(result: Result<T>, expected: &str) {
    assert_eq!(result.unwrap_err().code(), expected);
}

#[test]
fn resumes_after_partial_upload_and_browser_reopen() {
    let f = Fixture::open();
    let id = f.begin(b"abcdef");
    assert_eq!(format!("{id:?}"), "UploadId[<redacted>]");
    assert_eq!(f.files.append(&f.owner, &id, 0, b"abc").unwrap(), 3);
    assert!(!f.game.join("config/upload.toml").exists());
    assert_eq!(fs::read_dir(f.game.join("config")).unwrap().count(), 0);
    let reopened_db = StateDatabase::open(f.db.directory().clone(), f.clock.clone()).unwrap();
    let browser = FileBrowser::open(reopened_db, &f.game, &[]).unwrap();
    let id = UploadId::parse(id.as_str()).unwrap();
    browser.append(&f.owner, &id, 3, b"def").unwrap();
    let change = browser.finish(&f.owner, &id).unwrap();
    assert_eq!(change.sha256, sha(b"abcdef"));
    assert_eq!(
        fs::read(f.game.join("config/upload.toml")).unwrap(),
        b"abcdef"
    );
    assert_eq!((f.rows(), f.stages()), (0, 0));
    let entries = AuditLog::new(f.db.clone()).unwrap().page(0, 100).unwrap();
    assert_eq!(
        entries.iter().filter(|e| e.action == "files.write").count(),
        1
    );
    code(browser.finish(&f.owner, &id), "file_not_found");
}

#[test]
fn wrong_offset_and_different_duplicates_conflict_identical_duplicates_succeed() {
    let f = Fixture::open();
    let id = f.begin(b"abcdef");
    code(f.files.append(&f.owner, &id, 1, b"a"), "file_conflict");
    f.files.append(&f.owner, &id, 0, b"abc").unwrap();
    assert_eq!(f.files.append(&f.owner, &id, 0, b"abc").unwrap(), 3);
    assert_eq!(f.files.append(&f.owner, &id, 1, b"b").unwrap(), 3);
    code(f.files.append(&f.owner, &id, 0, b"xyz"), "file_conflict");
    code(f.files.append(&f.owner, &id, 2, b"cd"), "file_conflict");
    f.files.append(&f.owner, &id, 3, b"def").unwrap();
    f.files.finish(&f.owner, &id).unwrap();
}

#[test]
fn oversize_total_chunk_and_past_total_refuse_without_writing() {
    let f = Fixture::open();
    code(
        f.files.begin(
            &f.owner,
            &path(),
            &WritePrecondition::Absent,
            MAX_WRITE_BYTES as u64 + 1,
            &sha(b""),
        ),
        "file_too_large",
    );
    let id = f.begin(b"abc");
    code(
        f.files
            .append(&f.owner, &id, 0, &vec![0; MAX_UPLOAD_CHUNK_BYTES + 1]),
        "file_too_large",
    );
    code(f.files.append(&f.owner, &id, 0, b"abcd"), "file_too_large");
    code(
        f.files.append(&f.owner, &id, u64::MAX, b"a"),
        "file_too_large",
    );
    assert_eq!(
        f.db.directory()
            .uploads()
            .unwrap()
            .open(&id.name().unwrap())
            .unwrap()
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn incomplete_length_and_hash_mismatch_never_commit() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    f.files.append(&f.owner, &id, 0, b"a").unwrap();
    code(f.files.finish(&f.owner, &id), "file_conflict");
    f.files.append(&f.owner, &id, 1, b"xx").unwrap();
    code(f.files.finish(&f.owner, &id), "file_conflict");
    assert!(!f.game.join("config/upload.toml").exists());
    assert_eq!((f.rows(), f.stages()), (1, 1));
}

#[test]
fn precondition_changed_since_begin_conflicts_and_leaves_current_bytes() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    f.files.append(&f.owner, &id, 0, b"abc").unwrap();
    fs::write(f.game.join("config/upload.toml"), b"intervening").unwrap();
    code(f.files.finish(&f.owner, &id), "file_conflict");
    assert_eq!(
        fs::read(f.game.join("config/upload.toml")).unwrap(),
        b"intervening"
    );
    f.files.abort(&f.owner, &id).unwrap();
    let id = f
        .files
        .begin(
            &f.owner,
            &path(),
            &WritePrecondition::Matches(sha(b"intervening")),
            3,
            &sha(b"abc"),
        )
        .unwrap();
    f.files.append(&f.owner, &id, 0, b"abc").unwrap();
    fs::write(f.game.join("config/upload.toml"), b"newer").unwrap();
    code(f.files.finish(&f.owner, &id), "file_conflict");
    assert_eq!(
        fs::read(f.game.join("config/upload.toml")).unwrap(),
        b"newer"
    );
    assert_eq!(
        f.db.read(
            |c| Ok(c.query_row("SELECT count(*) FROM file_trash", [], |r| r
                .get::<_, i64>(0))?)
        )
        .unwrap(),
        0
    );
}

#[test]
fn successful_replacement_keeps_old_bytes_in_trash() {
    let f = Fixture::open();
    fs::write(f.game.join("config/upload.toml"), b"old").unwrap();
    let id = f
        .files
        .begin(
            &f.owner,
            &path(),
            &WritePrecondition::Matches(sha(b"old")),
            3,
            &sha(b"new"),
        )
        .unwrap();
    f.files.append(&f.owner, &id, 0, b"new").unwrap();
    let change = f.files.finish(&f.owner, &id).unwrap();
    let trash_id = change.trash.unwrap();
    let name = ArtifactName::new(&format!("{trash_id}.bin")).unwrap();
    let mut old = Vec::new();
    f.db.directory()
        .trash()
        .unwrap()
        .open(&name)
        .unwrap()
        .unwrap()
        .read_to_end(&mut old)
        .unwrap();
    assert_eq!(old, b"old");
    assert_eq!((f.rows(), f.stages()), (0, 0));
}

#[test]
fn only_beginning_subject_can_append_finish_or_abort() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    let other = seed(&f.db);
    code(f.files.append(&other, &id, 0, b"abc"), "file_not_found");
    code(f.files.finish(&other, &id), "file_not_found");
    code(f.files.abort(&other, &id), "file_not_found");
    let same_principal_other_device = Subject {
        principal: f.owner.principal.clone(),
        device: other.device,
    };
    code(
        f.files.abort(&same_principal_other_device, &id),
        "file_not_found",
    );
    assert_eq!((f.rows(), f.stages()), (1, 1));
}

#[test]
fn expiry_purge_removes_rows_stages_and_orphans_but_preserves_live_upload() {
    let f = Fixture::open();
    let browser = FileBrowser::open(f.db.clone(), &f.game, &[])
        .unwrap()
        .with_upload_policy(UploadPolicy::new(2, 3, 100, Duration::from_secs(1)).unwrap());
    let id = browser
        .begin(
            &f.owner,
            &path(),
            &WritePrecondition::Absent,
            3,
            &sha(b"abc"),
        )
        .unwrap();
    browser.append(&f.owner, &id, 0, b"a").unwrap();
    assert_eq!(browser.purge_expired_uploads().unwrap(), 0);
    f.clock.0.store(START + 1000, Ordering::SeqCst);
    code(browser.append(&f.owner, &id, 1, b"bc"), "file_not_found");
    let orphan = UploadId(random_id().unwrap()).name().unwrap();
    drop(
        f.db.directory()
            .uploads()
            .unwrap()
            .create_new(&orphan)
            .unwrap(),
    );
    assert_eq!(browser.purge_expired_uploads().unwrap(), 2);
    assert_eq!((f.rows(), f.stages()), (0, 0));
    assert_eq!(browser.purge_expired_uploads().unwrap(), 0);
}

#[test]
fn abort_cleans_stage_and_releases_reservation() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    f.files.append(&f.owner, &id, 0, b"a").unwrap();
    f.files.abort(&f.owner, &id).unwrap();
    assert_eq!((f.rows(), f.stages()), (0, 0));
    code(f.files.finish(&f.owner, &id), "file_not_found");
    assert!(!f.game.join("config/upload.toml").exists());
}

#[test]
fn quotas_bound_subject_global_and_total_reserved_bytes() {
    let f = Fixture::open();
    let browser = FileBrowser::open(f.db.clone(), &f.game, &[])
        .unwrap()
        .with_upload_policy(UploadPolicy::new(1, 2, 6, Duration::from_secs(60)).unwrap());
    let begin = |s: &Subject, size| {
        browser.begin(s, &path(), &WritePrecondition::Absent, size, &sha(b"abc"))
    };
    let id = begin(&f.owner, 3).unwrap();
    code(begin(&f.owner, 0), "file_upload_full");
    let other = seed(&f.db);
    code(begin(&other, 4), "file_upload_full");
    let other_id = begin(&other, 3).unwrap();
    let third = seed(&f.db);
    code(begin(&third, 0), "file_upload_full");
    browser.abort(&f.owner, &id).unwrap();
    begin(&third, 3).unwrap();
    browser.abort(&other, &other_id).unwrap();
}

#[test]
fn empty_upload_is_valid_and_invalid_policy_or_digest_refuses() {
    let f = Fixture::open();
    code(
        UploadPolicy::new(0, 1, 1, Duration::from_secs(1)),
        "file_limit_invalid",
    );
    code(
        f.files
            .begin(&f.owner, &path(), &WritePrecondition::Absent, 0, "bad"),
        "file_path_invalid",
    );
    let id = f.begin(b"");
    f.files.finish(&f.owner, &id).unwrap();
    assert_eq!(fs::read(f.game.join("config/upload.toml")).unwrap(), b"");
}

#[test]
fn failed_success_audit_rolls_back_write_and_upload_retirement() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    f.files.append(&f.owner, &id, 0, b"abc").unwrap();
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER fail_upload_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT, 'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(f.files.finish(&f.owner, &id).is_err());
    assert!(!f.game.join("config/upload.toml").exists());
    assert_eq!((f.rows(), f.stages()), (1, 1));
}

#[test]
fn fresh_authority_is_required_at_begin_append_and_finish() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    f.files.append(&f.owner, &id, 0, b"abc").unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE principals SET status='disabled' WHERE id=?",
            [f.owner.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        f.files
            .begin(
                &f.owner,
                &path(),
                &WritePrecondition::Absent,
                3,
                &sha(b"abc")
            )
            .is_err()
    );
    assert!(f.files.append(&f.owner, &id, 0, b"abc").is_err());
    assert!(f.files.finish(&f.owner, &id).is_err());
    assert!(!f.game.join("config/upload.toml").exists());
    assert_eq!(f.rows(), 1);
}

#[test]
fn maintenance_removes_missing_stages_without_touching_live_bytes() {
    let f = Fixture::open();
    let missing = f.begin(b"abc");
    let live = f.begin(b"def");
    f.files.append(&f.owner, &live, 0, b"def").unwrap();
    f.db.directory()
        .uploads()
        .unwrap()
        .remove(&missing.name().unwrap())
        .unwrap();
    assert_eq!(f.files.purge_expired_uploads().unwrap(), 1);
    assert_eq!((f.rows(), f.stages()), (1, 1));
    f.files.finish(&f.owner, &live).unwrap();
    assert_eq!(fs::read(f.game.join("config/upload.toml")).unwrap(), b"def");
}

#[test]
fn separate_browsers_share_quota_admission() {
    let f = Fixture::open();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let browser = FileBrowser::open(f.db.clone(), &f.game, &[])
            .unwrap()
            .with_upload_policy(UploadPolicy::new(1, 1, 3, Duration::from_secs(60)).unwrap());
        let subject = f.owner.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            browser.begin(
                &subject,
                &path(),
                &WritePrecondition::Absent,
                3,
                &sha(b"abc"),
            )
        }));
    }
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(FileError::UploadFull)))
            .count(),
        1
    );
    assert_eq!((f.rows(), f.stages()), (1, 1));
}

#[test]
fn linked_staging_is_refused_without_modifying_external_bytes() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    let precious = f.game.join("config/precious.toml");
    fs::write(&precious, b"precious").unwrap();
    let stage =
        f.db.directory()
            .path()
            .join("uploads")
            .join(id.name().unwrap().as_str());
    fs::remove_file(&stage).unwrap();
    fs::hard_link(&precious, &stage).unwrap();
    code(f.files.append(&f.owner, &id, 0, b"abc"), "file_path_linked");
    code(f.files.finish(&f.owner, &id), "file_path_linked");
    assert_eq!(fs::read(&precious).unwrap(), b"precious");
    assert!(!f.game.join("config/upload.toml").exists());
}

#[test]
fn removed_replacement_target_is_a_precondition_conflict() {
    let f = Fixture::open();
    let target = f.game.join("config/upload.toml");
    fs::write(&target, b"old").unwrap();
    let id = f
        .files
        .begin(
            &f.owner,
            &path(),
            &WritePrecondition::Matches(sha(b"old")),
            3,
            &sha(b"new"),
        )
        .unwrap();
    f.files.append(&f.owner, &id, 0, b"new").unwrap();
    fs::remove_file(&target).unwrap();
    code(f.files.finish(&f.owner, &id), "file_conflict");
    assert!(!target.exists());
    assert_eq!((f.rows(), f.stages()), (1, 1));
}

#[test]
fn abort_releases_staging_and_quota_when_destination_parent_is_removed_or_renamed() {
    for rename in [false, true] {
        let f = Fixture::open();
        let id = f.begin(b"abc");
        f.files.append(&f.owner, &id, 0, b"a").unwrap();
        let parent = f.game.join("config");
        if rename {
            fs::rename(&parent, f.game.join("renamed")).unwrap();
        } else {
            fs::remove_dir(&parent).unwrap();
        }
        f.files.abort(&f.owner, &id).unwrap();
        assert_eq!((f.rows(), f.stages()), (0, 0));
        assert!(!f.game.join("config/upload.toml").exists());
    }
}

#[test]
fn abort_requires_live_authority_but_can_clean_an_expired_owned_upload() {
    let f = Fixture::open();
    let id = f.begin(b"abc");
    f.db.write(|c| {
        c.execute(
            "UPDATE principals SET status='disabled' WHERE id=?",
            [f.owner.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(f.files.abort(&f.owner, &id).is_err());
    assert_eq!((f.rows(), f.stages()), (1, 1));
    f.db.write(|c| {
        c.execute(
            "UPDATE principals SET status='active' WHERE id=?",
            [f.owner.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    f.clock.0.store(START + 3_600_000, Ordering::SeqCst);
    f.files.abort(&f.owner, &id).unwrap();
    assert_eq!((f.rows(), f.stages()), (0, 0));
}
