use super::*;
use crate::backups::store::{LocalStore, ObjectInfo};
use std::sync::{Arc, Mutex};

/// Stored bytes and modification time, by key.
type Objects = BTreeMap<String, (Vec<u8>, i64)>;

/// In-memory store whose object ages can be shifted to exercise prune grace periods.
#[derive(Clone, Default)]
struct MemStore {
    objects: Arc<Mutex<Objects>>,
}
impl MemStore {
    fn age_everything(&self, by_ms: i64) {
        for (_, modified) in self.objects.lock().unwrap().values_mut() {
            *modified -= by_ms;
        }
    }
    fn keys(&self, prefix: &str) -> usize {
        self.objects
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .count()
    }
    fn corrupt_one_chunk(&self) {
        let mut objects = self.objects.lock().unwrap();
        let (_, (bytes, _)) = objects
            .iter_mut()
            .find(|(k, _)| k.starts_with("chunks/"))
            .unwrap();
        *bytes = compress(b"tampered");
    }
}
impl Store for MemStore {
    fn put(&self, key: &str, bytes: &[u8]) -> crate::backups::store::Result<()> {
        self.objects
            .lock()
            .unwrap()
            .insert(key.into(), (bytes.to_vec(), now_ms()));
        Ok(())
    }
    fn get(&self, key: &str) -> crate::backups::store::Result<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .map(|(b, _)| b.clone())
            .ok_or(StoreError::NotFound)
    }
    fn exists(&self, key: &str) -> crate::backups::store::Result<bool> {
        Ok(self.objects.lock().unwrap().contains_key(key))
    }
    fn list(&self, prefix: &str, limit: usize) -> crate::backups::store::Result<Vec<ObjectInfo>> {
        let objects = self.objects.lock().unwrap();
        let found: Vec<_> = objects
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, (b, m))| ObjectInfo {
                key: k.clone(),
                size: b.len() as u64,
                modified_ms: *m,
            })
            .collect();
        if found.len() > limit {
            return Err(StoreError::TooLarge);
        }
        Ok(found)
    }
    fn delete(&self, key: &str) -> crate::backups::store::Result<()> {
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
}

fn game() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("world/region")).unwrap();
    fs::write(root.join("world/level.dat"), b"level").unwrap();
    // A multi-chunk region file with distinct content per chunk.
    let region: Vec<u8> = (0..(CHUNK_SIZE * 3 + 100))
        .map(|i| (i / 7 % 251) as u8)
        .collect();
    fs::write(root.join("world/region/r.0.0.mca"), &region).unwrap();
    fs::create_dir_all(root.join("logs")).unwrap();
    fs::write(root.join("logs/latest.log"), b"log").unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    temp
}

fn tags(profile: &str) -> Vec<String> {
    vec!["vesta".into(), format!("profile={profile}")]
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let (_, files) = walk(root, &Ignore::default()).unwrap();
    files
        .into_iter()
        .map(|(p, _, _)| {
            let bytes = fs::read(native(root, &p)).unwrap();
            (p, bytes)
        })
        .collect()
}

#[test]
fn backup_restore_round_trip_with_ignore_and_empty_directories() {
    let source = game();
    let repo = tempfile::tempdir().unwrap();
    let engine = Engine::new(Box::new(LocalStore::new(repo.path().join("r"))));
    assert_eq!(
        engine.snapshots("vesta").unwrap_err(),
        EngineError::Failed,
        "uninitialized"
    );
    engine.init().unwrap();
    engine.init().unwrap();
    let ignore = Ignore::parse("logs/\n").unwrap();
    let summary = engine
        .backup(source.path(), &ignore, &tags("p"), Some("profile=p"))
        .unwrap();
    assert!(valid_snapshot_id(&summary.snapshot_id));
    assert_eq!(summary.files_new, 2);
    let target = tempfile::tempdir().unwrap();
    engine
        .restore(&summary.snapshot_id, &target.path().join("out"), &[])
        .unwrap();
    let mut expected = tree(source.path());
    expected.remove("logs/latest.log");
    assert_eq!(tree(&target.path().join("out")), expected);
    assert!(target.path().join("out/empty").is_dir());
    // Selected paths only.
    engine
        .restore(
            &summary.snapshot_id,
            &target.path().join("partial"),
            &["world/level.dat".into()],
        )
        .unwrap();
    assert_eq!(tree(&target.path().join("partial")).len(), 1);
    assert_eq!(
        engine.restore(
            &summary.snapshot_id,
            &target.path().join("x"),
            &["world/*".into()]
        ),
        Err(EngineError::Invalid)
    );
}

#[test]
fn unchanged_files_and_chunks_are_deduplicated() {
    let source = game();
    let store = MemStore::default();
    let engine = Engine::new(Box::new(store.clone()));
    engine.init().unwrap();
    let none = Ignore::default();
    let first = engine
        .backup(source.path(), &none, &tags("p"), Some("profile=p"))
        .unwrap();
    let chunks = store.keys("chunks/");
    // Nothing changed: every file is reused without reading, no new chunks.
    let second = engine
        .backup(source.path(), &none, &tags("p"), Some("profile=p"))
        .unwrap();
    assert_eq!(second.files_unmodified, 3);
    assert_eq!(store.keys("chunks/"), chunks);
    assert!(second.data_added < first.data_added);
    // Rewrite one 1 MiB region of the file in place: only that chunk is new.
    let path = source.path().join("world/region/r.0.0.mca");
    let mut region = fs::read(&path).unwrap();
    region[CHUNK_SIZE + 5] ^= 0xff;
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&path, &region).unwrap();
    let third = engine
        .backup(source.path(), &none, &tags("p"), Some("profile=p"))
        .unwrap();
    assert_eq!(third.files_changed, 1);
    assert_eq!(store.keys("chunks/"), chunks + 1);
    let target = tempfile::tempdir().unwrap();
    engine
        .restore(&third.snapshot_id, target.path(), &[])
        .unwrap();
    assert_eq!(
        fs::read(target.path().join("world/region/r.0.0.mca")).unwrap(),
        region
    );
}

#[test]
fn tags_retention_and_prune_keep_only_what_is_referenced() {
    let source = game();
    let store = MemStore::default();
    let engine = Engine::new(Box::new(store.clone()));
    engine.init().unwrap();
    let none = Ignore::default();
    let candidate = vec!["vesta".to_owned(), "candidate=p".to_owned()];
    let mut ids = Vec::new();
    for round in 0..3u8 {
        fs::write(source.path().join("world/level.dat"), [round; 10]).unwrap();
        let summary = engine
            .backup(source.path(), &none, &candidate, Some("profile=p"))
            .unwrap();
        engine
            .tag_add(&summary.snapshot_id, &["profile=p".into()])
            .unwrap();
        ids.push(summary.snapshot_id);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // An unpromoted candidate is not a restore point.
    let orphan = engine
        .backup(source.path(), &none, &candidate, Some("profile=p"))
        .unwrap();
    let listed: Vec<String> = engine
        .snapshots("profile=p")
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(listed, ids);
    store.age_everything(2 * PRUNE_GRACE_MS);
    engine
        .forget(
            "profile=p",
            Retention {
                keep_last: 2,
                ..Retention::default()
            },
        )
        .unwrap();
    let listed: Vec<String> = engine
        .snapshots("profile=p")
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(listed, ids[1..]);
    // The young orphan candidate survives the first prune; after a day it is removed.
    assert!(
        store
            .exists(&format!("snapshots/{}", orphan.snapshot_id))
            .unwrap()
    );
    store.age_everything(UNPROMOTED_TTL_MS + 1);
    engine.prune().unwrap();
    assert!(
        !store
            .exists(&format!("snapshots/{}", orphan.snapshot_id))
            .unwrap()
    );
    engine.check(Some(100)).unwrap();
    // Every remaining snapshot still restores exactly.
    for id in &ids[1..] {
        let target = tempfile::tempdir().unwrap();
        engine.restore(id, target.path(), &[]).unwrap();
    }
}

#[test]
fn corruption_is_detected_by_check_and_restore() {
    let source = game();
    let store = MemStore::default();
    let engine = Engine::new(Box::new(store.clone()));
    engine.init().unwrap();
    let summary = engine
        .backup(source.path(), &Ignore::default(), &tags("p"), None)
        .unwrap();
    engine.check(None).unwrap();
    store.corrupt_one_chunk();
    assert_eq!(engine.check(Some(100)), Err(EngineError::Corrupt));
    let target = tempfile::tempdir().unwrap();
    assert_eq!(
        engine.restore(&summary.snapshot_id, target.path(), &[]),
        Err(EngineError::Corrupt)
    );
}

#[test]
fn a_tampered_manifest_cannot_write_outside_the_target() {
    let store = MemStore::default();
    let engine = Engine::new(Box::new(store.clone()));
    engine.init().unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    let manifest = Manifest {
        version: 1,
        id: id.into(),
        time_ms: 1,
        dirs: vec![],
        files: vec![FileEntry {
            path: "../escape.txt".into(),
            size: 0,
            modified_ms: 0,
            chunks: vec![],
        }],
    };
    store
        .put(
            &format!("snapshots/{id}"),
            &compress(&serde_json::to_vec(&manifest).unwrap()),
        )
        .unwrap();
    let target = tempfile::tempdir().unwrap();
    assert_eq!(
        engine.restore(id, &target.path().join("t"), &[]),
        Err(EngineError::Corrupt)
    );
    assert!(!target.path().join("escape.txt").exists());
}

#[test]
fn retention_buckets_and_dates() {
    let day = 86_400_000i64;
    // Two snapshots per day for ten days.
    let snapshots: Vec<(String, i64)> = (0..20)
        .map(|i| (format!("{i:032x}"), 1_800_000_000_000 + i * day / 2))
        .collect();
    let keep = keep_set(
        &snapshots,
        Retention {
            keep_daily: 3,
            ..Retention::default()
        },
    );
    assert_eq!(keep.len(), 3);
    assert!(keep.contains(&format!("{:032x}", 19)));
    let keep = keep_set(
        &snapshots,
        Retention {
            keep_last: 1,
            keep_weekly: 2,
            ..Retention::default()
        },
    );
    assert!(keep.len() >= 2 && keep.len() <= 3);
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(1_791_115_200_000), "2026-10-04T12:00:00Z");
    assert_eq!(civil(-1), (1969, 12, 31));
}
