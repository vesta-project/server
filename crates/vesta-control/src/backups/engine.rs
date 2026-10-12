//! Vesta's built-in deduplicating backup engine over any [`Store`].
//!
//! Repository layout (all keys from [`super::store::valid_key`]):
//! - `config.json`: format marker and chunk size.
//! - `chunks/<2 hex>/<64 hex>`: file content split into fixed 1 MiB chunks, keyed by
//!   the SHA-256 of the uncompressed bytes; stored once, deflate-compressed when smaller.
//!   Fixed chunks suit Minecraft region files, which are rewritten in place sector by
//!   sector, so unchanged regions and unchanged parts of changed regions are reused.
//! - `snapshots/<id>`: compressed JSON manifest (paths, sizes, modification times and
//!   chunk hashes). Written only after every chunk it references.
//! - `tags/<id>`: small mutable JSON (time and tags); the snapshot listing.
//!
//! No encryption: repositories are meant for owner-controlled disks or buckets. Data
//! integrity is checked by hash on restore and by `check`. A repository has a single
//! writer (one agent); concurrent writers are not coordinated.
use super::{
    ignore::Ignore,
    store::{MAX_OBJECT_BYTES, Store, StoreError},
};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const CHUNK_SIZE: usize = 1024 * 1024;
const FORMAT: &str = "vesta-backup";
const MAX_FILES: usize = 1_000_000;
const MAX_CHUNKS_LISTED: usize = 10_000_000;
pub const MAX_SNAPSHOTS: usize = 10_000;
/// Unreferenced chunks younger than this are kept: a backup may still be writing them.
const PRUNE_GRACE_MS: i64 = 60 * 60 * 1000;
/// Snapshots never promoted into a profile are removed after this long.
const UNPROMOTED_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const PROFILE_PREFIX: &str = "profile=";
/// Pre-restore safety snapshots, kept by their own count retention rather than a profile.
pub const SAFETY_PREFIX: &str = "safety=";

mod apply;
pub use apply::{ApplyError, ApplySummary};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    /// The repository storage could not be reached or written.
    Unavailable,
    /// The repository is missing, uninitialized or holds unexpected data.
    Failed,
    /// Some source files could not be read.
    Incomplete,
    /// Repository contents failed verification (missing or corrupt objects).
    Corrupt,
    /// A bound was exceeded (files, chunks, snapshots, object size).
    TooLarge,
    Invalid,
    /// The snapshot does not exist (or has no listing record).
    Missing,
    /// A requested restore path is not in the snapshot.
    NotInSnapshot,
    /// The restore target holds a link, special file or conflicting entry.
    TargetUnsafe,
    /// A backup would store more than its byte budget (repository size limit or the
    /// repository disk's free-space reserve).
    LimitReached,
    /// The disk receiving restored files would drop below its free-space reserve.
    DiskSpaceLow,
}
impl EngineError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "backup_storage_unavailable",
            Self::Failed => "backup_repository_unavailable",
            Self::Incomplete => "backup_source_incomplete",
            Self::Corrupt => "backup_repository_corrupt",
            Self::TooLarge => "backup_too_large",
            Self::Invalid => "backup_invalid_request",
            Self::Missing => "backup_snapshot_unknown",
            Self::NotInSnapshot => "backup_restore_path_not_in_snapshot",
            Self::TargetUnsafe => "backup_restore_target_unsafe",
            Self::LimitReached => "backup_storage_limit_reached",
            Self::DiskSpaceLow => "backup_disk_space_low",
        }
    }
}
impl From<StoreError> for EngineError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound | StoreError::Invalid => Self::Corrupt,
            StoreError::Unavailable => Self::Unavailable,
            StoreError::TooLarge => Self::TooLarge,
        }
    }
}
pub type Result<T> = std::result::Result<T, EngineError>;

/// Retention for one profile; zero means "not used".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Retention {
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
}
impl Retention {
    pub fn is_empty(&self) -> bool {
        self.keep_last == 0
            && self.keep_daily == 0
            && self.keep_weekly == 0
            && self.keep_monthly == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupSummary {
    pub snapshot_id: String,
    pub files_new: u64,
    pub files_changed: u64,
    pub files_unmodified: u64,
    /// Compressed bytes newly written to the repository.
    pub data_added: u64,
    pub total_bytes_processed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub id: String,
    /// RFC 3339 UTC time.
    pub time: String,
    pub tags: Vec<String>,
}

/// Snapshot identifiers are 32 lowercase hex characters.
pub fn valid_snapshot_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Restore paths are literal relative paths: wildcard syntax is refused so a request
/// always means exactly that path.
pub fn literal_path(path: &str) -> bool {
    !path
        .bytes()
        .any(|b| matches!(b, b'*' | b'?' | b'[' | b']' | b'\\' | b'{' | b'}'))
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 80
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'=' | b'-' | b'_' | b'.'))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn chunk_key(hash: &str) -> String {
    format!("chunks/{}/{hash}", &hash[..2])
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoConfig {
    format: String,
    version: u8,
    chunk_size: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    size: u64,
    modified_ms: i64,
    chunks: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    id: String,
    time_ms: i64,
    dirs: Vec<String>,
    files: Vec<FileEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagRecord {
    time_ms: i64,
    tags: Vec<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Hinnant).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s / 60) % 60,
        s % 60
    )
}

fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(vec![1u8], Compression::default());
    if encoder.write_all(bytes).is_ok()
        && let Ok(compressed) = encoder.finish()
        && compressed.len() < bytes.len() + 1
    {
        return compressed;
    }
    let mut stored = Vec::with_capacity(bytes.len() + 1);
    stored.push(0);
    stored.extend_from_slice(bytes);
    stored
}

fn decompress(object: &[u8], limit: usize) -> Result<Vec<u8>> {
    let (method, payload) = object.split_first().ok_or(EngineError::Corrupt)?;
    match method {
        0 if payload.len() <= limit => Ok(payload.to_vec()),
        1 => {
            let mut out = Vec::new();
            DeflateDecoder::new(payload)
                .take(limit as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|_| EngineError::Corrupt)?;
            if out.len() > limit {
                return Err(EngineError::TooLarge);
            }
            Ok(out)
        }
        _ => Err(EngineError::Corrupt),
    }
}

fn hash_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub struct Engine {
    store: Box<dyn Store>,
}
impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Engine")
    }
}

impl Engine {
    pub fn new(store: Box<dyn Store>) -> Self {
        Self { store }
    }

    /// Create the repository; an existing valid repository is accepted unchanged.
    pub fn init(&self) -> Result<()> {
        match self.store.get("config.json") {
            Ok(_) => self.require_initialized(),
            Err(StoreError::NotFound) => {
                let config = RepoConfig {
                    format: FORMAT.into(),
                    version: 1,
                    chunk_size: CHUNK_SIZE,
                };
                let bytes = serde_json::to_vec(&config).map_err(|_| EngineError::Failed)?;
                self.store.put("config.json", &bytes)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn require_initialized(&self) -> Result<()> {
        let bytes = match self.store.get("config.json") {
            Ok(bytes) => bytes,
            Err(StoreError::NotFound) => return Err(EngineError::Failed),
            Err(error) => return Err(error.into()),
        };
        let config: RepoConfig = serde_json::from_slice(&bytes).map_err(|_| EngineError::Failed)?;
        if config.format != FORMAT || config.version != 1 || config.chunk_size != CHUNK_SIZE {
            return Err(EngineError::Failed);
        }
        Ok(())
    }

    fn manifest(&self, id: &str) -> Result<Manifest> {
        if !valid_snapshot_id(id) {
            return Err(EngineError::Invalid);
        }
        let bytes = decompress(
            &self.store.get(&format!("snapshots/{id}"))?,
            MAX_OBJECT_BYTES * 4,
        )?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| EngineError::Corrupt)?;
        if manifest.version != 1 || manifest.id != id {
            return Err(EngineError::Corrupt);
        }
        Ok(manifest)
    }

    /// Every snapshot with its tag record (snapshots without one are unpromoted crash
    /// leftovers and are skipped here).
    fn all(&self) -> Result<Vec<(String, TagRecord)>> {
        let mut out = Vec::new();
        for object in self.store.list("tags/", MAX_SNAPSHOTS)? {
            let Some(id) = object.key.strip_prefix("tags/") else {
                continue;
            };
            if !valid_snapshot_id(id) {
                continue;
            }
            let record: TagRecord = match self.store.get(&object.key) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| EngineError::Corrupt)?,
                Err(StoreError::NotFound) => continue,
                Err(error) => return Err(error.into()),
            };
            out.push((id.to_owned(), record));
        }
        out.sort_by_key(|(_, record)| record.time_ms);
        Ok(out)
    }

    /// Snapshots carrying `tag`, oldest first.
    pub fn snapshots(&self, tag: &str) -> Result<Vec<Snapshot>> {
        if !valid_tag(tag) {
            return Err(EngineError::Invalid);
        }
        self.require_initialized()?;
        Ok(self
            .all()?
            .into_iter()
            .filter(|(_, record)| record.tags.iter().any(|t| t == tag))
            .map(|(id, record)| Snapshot {
                id,
                time: rfc3339(record.time_ms),
                tags: record.tags,
            })
            .collect())
    }

    /// Back up `root`. Files unchanged since the newest snapshot tagged `parent_tag`
    /// (same size and modification time) reuse its chunks without being read.
    pub fn backup(
        &self,
        root: &Path,
        ignore: &Ignore,
        tags: &[String],
        parent_tag: Option<&str>,
    ) -> Result<BackupSummary> {
        self.backup_within(root, ignore, tags, parent_tag, None)
    }

    /// As `backup`, storing at most `budget` new bytes: past it the backup stops with
    /// `LimitReached`. Chunks it already stored are unreferenced and pruned later.
    pub fn backup_within(
        &self,
        root: &Path,
        ignore: &Ignore,
        tags: &[String],
        parent_tag: Option<&str>,
        budget: Option<u64>,
    ) -> Result<BackupSummary> {
        let within =
            |added: u64, next: usize| budget.is_none_or(|b| added.saturating_add(next as u64) <= b);
        if tags.iter().any(|t| !valid_tag(t)) {
            return Err(EngineError::Invalid);
        }
        self.require_initialized()?;
        let parent: BTreeMap<String, FileEntry> = match parent_tag {
            Some(tag) => match self.snapshots(tag)?.last() {
                Some(latest) => self
                    .manifest(&latest.id)?
                    .files
                    .into_iter()
                    .map(|f| (f.path.clone(), f))
                    .collect(),
                None => BTreeMap::new(),
            },
            None => BTreeMap::new(),
        };
        let mut known: HashSet<String> = self
            .store
            .list("chunks/", MAX_CHUNKS_LISTED)?
            .into_iter()
            .filter_map(|o| o.key.rsplit('/').next().map(str::to_owned))
            .collect();
        let (dirs, files) = walk(root, ignore)?;
        let mut summary = BackupSummary {
            snapshot_id: String::new(),
            files_new: 0,
            files_changed: 0,
            files_unmodified: 0,
            data_added: 0,
            total_bytes_processed: 0,
        };
        let mut entries = Vec::with_capacity(files.len());
        let mut buffer = vec![0u8; CHUNK_SIZE];
        for (relative, size, modified_ms) in files {
            summary.total_bytes_processed += size;
            if let Some(previous) = parent.get(&relative)
                && previous.size == size
                && previous.modified_ms == modified_ms
                && previous.chunks.iter().all(|h| known.contains(h))
            {
                summary.files_unmodified += 1;
                entries.push(previous.clone());
                continue;
            }
            if parent.contains_key(&relative) {
                summary.files_changed += 1;
            } else {
                summary.files_new += 1;
            }
            let mut file =
                fs::File::open(native(root, &relative)).map_err(|_| EngineError::Incomplete)?;
            let mut chunks = Vec::new();
            let mut read_size = 0u64;
            loop {
                let mut filled = 0;
                while filled < CHUNK_SIZE {
                    match file.read(&mut buffer[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => return Err(EngineError::Incomplete),
                    }
                }
                if filled == 0 {
                    break;
                }
                read_size += filled as u64;
                let hash = hash_hex(&buffer[..filled]);
                if !known.contains(&hash) {
                    let object = compress(&buffer[..filled]);
                    if !within(summary.data_added, object.len()) {
                        return Err(EngineError::LimitReached);
                    }
                    self.store.put(&chunk_key(&hash), &object)?;
                    summary.data_added += object.len() as u64;
                    known.insert(hash.clone());
                }
                chunks.push(hash);
                if filled < CHUNK_SIZE {
                    break;
                }
            }
            entries.push(FileEntry {
                path: relative,
                size: read_size,
                modified_ms,
                chunks,
            });
        }
        let mut id_bytes = [0u8; 16];
        getrandom::fill(&mut id_bytes).map_err(|_| EngineError::Failed)?;
        let id = hex::encode(id_bytes);
        let time_ms = now_ms();
        let manifest = Manifest {
            version: 1,
            id: id.clone(),
            time_ms,
            dirs,
            files: entries,
        };
        let json = serde_json::to_vec(&manifest).map_err(|_| EngineError::Failed)?;
        let object = compress(&json);
        summary.data_added += object.len() as u64;
        // Chunks first, then the manifest, then the listing record.
        if !within(summary.data_added, object.len()) {
            return Err(EngineError::LimitReached);
        }
        self.store.put(&format!("snapshots/{id}"), &object)?;
        self.write_tags(
            &id,
            &TagRecord {
                time_ms,
                tags: tags.to_vec(),
            },
        )?;
        summary.snapshot_id = id;
        Ok(summary)
    }

    fn write_tags(&self, id: &str, record: &TagRecord) -> Result<()> {
        let bytes = serde_json::to_vec(record).map_err(|_| EngineError::Failed)?;
        self.store.put(&format!("tags/{id}"), &bytes)?;
        Ok(())
    }

    /// The tags of one snapshot.
    pub fn snapshot_tags(&self, snapshot: &str) -> Result<Vec<String>> {
        if !valid_snapshot_id(snapshot) {
            return Err(EngineError::Invalid);
        }
        self.require_initialized()?;
        let bytes = match self.store.get(&format!("tags/{snapshot}")) {
            Ok(bytes) => bytes,
            Err(StoreError::NotFound) => return Err(EngineError::Missing),
            Err(error) => return Err(error.into()),
        };
        let record: TagRecord = serde_json::from_slice(&bytes).map_err(|_| EngineError::Corrupt)?;
        Ok(record.tags)
    }

    /// Add tags to one snapshot (promotes a checked candidate into its profile).
    pub fn tag_add(&self, snapshot: &str, tags: &[String]) -> Result<()> {
        if !valid_snapshot_id(snapshot) || tags.is_empty() || tags.iter().any(|t| !valid_tag(t)) {
            return Err(EngineError::Invalid);
        }
        let key = format!("tags/{snapshot}");
        let mut record: TagRecord =
            serde_json::from_slice(&self.store.get(&key)?).map_err(|_| EngineError::Corrupt)?;
        for tag in tags {
            if !record.tags.contains(tag) {
                record.tags.push(tag.clone());
            }
        }
        self.write_tags(snapshot, &record)
    }

    /// Bytes stored in the repository (every object, including unreferenced chunks that
    /// a later prune would remove).
    pub fn usage(&self) -> Result<u64> {
        self.require_initialized()?;
        let mut total = 0u64;
        for prefix in ["chunks/", "snapshots/", "tags/"] {
            for object in self.store.list(prefix, MAX_CHUNKS_LISTED)? {
                total = total.saturating_add(object.size);
            }
        }
        Ok(total)
    }

    /// How many snapshots carry each tag.
    pub fn tag_counts(&self) -> Result<BTreeMap<String, usize>> {
        self.require_initialized()?;
        let mut counts = BTreeMap::new();
        for (_, record) in self.all()? {
            for tag in record.tags {
                *counts.entry(tag).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    /// How many snapshots tagged `tag` would remain if one more were taken now and
    /// `retention` then applied (no retention keeps them all).
    pub fn retained_after_backup(&self, tag: &str, retention: Retention) -> Result<usize> {
        if !valid_tag(tag) {
            return Err(EngineError::Invalid);
        }
        self.require_initialized()?;
        let mut snapshots: Vec<(String, i64)> = self
            .all()?
            .into_iter()
            .filter(|(_, r)| r.tags.iter().any(|t| t == tag))
            .map(|(id, r)| (id, r.time_ms))
            .collect();
        let newest = snapshots.iter().map(|(_, t)| *t).max().unwrap_or(0);
        // Not a snapshot ID, so it can never collide with one.
        snapshots.push(("next".into(), now_ms().max(newest + 1)));
        Ok(if retention.is_empty() {
            snapshots.len()
        } else {
            keep_set(&snapshots, retention).len()
        })
    }

    /// Apply retention to the snapshots tagged `tag`, then prune unreferenced data.
    pub fn forget(&self, tag: &str, retention: Retention) -> Result<()> {
        if !valid_tag(tag) || retention.is_empty() {
            return Err(EngineError::Invalid);
        }
        self.require_initialized()?;
        let snapshots: Vec<(String, i64)> = self
            .all()?
            .into_iter()
            .filter(|(_, r)| r.tags.iter().any(|t| t == tag))
            .map(|(id, r)| (id, r.time_ms))
            .collect();
        let keep = keep_set(&snapshots, retention);
        for (id, _) in &snapshots {
            if !keep.contains(id) {
                self.remove_snapshot(id)?;
            }
        }
        self.prune()
    }

    fn remove_snapshot(&self, id: &str) -> Result<()> {
        // Listing record first, so a crash never leaves a listed snapshot without data.
        self.store.delete(&format!("tags/{id}"))?;
        self.store.delete(&format!("snapshots/{id}"))?;
        Ok(())
    }

    /// Remove snapshots never promoted into a profile or kept as a safety snapshot
    /// (after a day), then every chunk
    /// no remaining manifest references (after a grace period).
    pub fn prune(&self) -> Result<()> {
        let now = now_ms();
        let records: BTreeMap<String, TagRecord> = self.all()?.into_iter().collect();
        let mut referenced = BTreeSet::new();
        for object in self.store.list("snapshots/", MAX_SNAPSHOTS)? {
            let Some(id) = object.key.strip_prefix("snapshots/").map(str::to_owned) else {
                continue;
            };
            if !valid_snapshot_id(&id) {
                continue;
            }
            let promoted = records.get(&id).is_some_and(|r| {
                r.tags
                    .iter()
                    .any(|t| t.starts_with(PROFILE_PREFIX) || t.starts_with(SAFETY_PREFIX))
            });
            // The store's clock, like the chunk grace period below.
            if !promoted && now - object.modified_ms > UNPROMOTED_TTL_MS {
                self.remove_snapshot(&id)?;
                continue;
            }
            for file in self.manifest(&id)?.files {
                referenced.extend(file.chunks);
            }
        }
        for object in self.store.list("chunks/", MAX_CHUNKS_LISTED)? {
            let Some(hash) = object.key.rsplit('/').next() else {
                continue;
            };
            if valid_hash(hash)
                && !referenced.contains(hash)
                && now - object.modified_ms > PRUNE_GRACE_MS
            {
                self.store.delete(&object.key)?;
            }
        }
        Ok(())
    }

    /// Verify every listed snapshot: its manifest parses and every chunk exists. With
    /// `read_data_percent`, that share of chunks is also read back and re-hashed.
    pub fn check(&self, read_data_percent: Option<u8>) -> Result<()> {
        if read_data_percent.is_some_and(|p| !(1..=100).contains(&p)) {
            return Err(EngineError::Invalid);
        }
        self.require_initialized()?;
        let existing: HashSet<String> = self
            .store
            .list("chunks/", MAX_CHUNKS_LISTED)?
            .into_iter()
            .filter_map(|o| o.key.rsplit('/').next().map(str::to_owned))
            .collect();
        let mut referenced = BTreeSet::new();
        for (id, _) in self.all()? {
            for file in self.manifest(&id)?.files {
                for hash in file.chunks {
                    if !valid_hash(&hash) || !existing.contains(&hash) {
                        return Err(EngineError::Corrupt);
                    }
                    referenced.insert(hash);
                }
            }
        }
        if let Some(percent) = read_data_percent {
            for hash in referenced {
                // Deterministic sample by hash prefix.
                let bucket = u8::from_str_radix(&hash[..2], 16).unwrap_or(0) as u32 * 100 / 256;
                if bucket < u32::from(percent) {
                    let data = decompress(&self.store.get(&chunk_key(&hash))?, CHUNK_SIZE)?;
                    if hash_hex(&data) != hash {
                        return Err(EngineError::Corrupt);
                    }
                }
            }
        }
        Ok(())
    }

    /// Restore a snapshot into `target` (which must be empty or absent), optionally only
    /// files at or under the given literal relative paths. Every chunk is re-hashed.
    pub fn restore(&self, snapshot: &str, target: &Path, includes: &[String]) -> Result<()> {
        if includes
            .iter()
            .any(|p| !literal_path(p) || crate::files::RelativePath::parse(p).is_err())
        {
            return Err(EngineError::Invalid);
        }
        let manifest = self.manifest(snapshot)?;
        let selected = |path: &str| {
            includes.is_empty()
                || includes
                    .iter()
                    .any(|inc| path == inc || path.starts_with(&format!("{inc}/")))
        };
        for dir in &manifest.dirs {
            if selected(dir) {
                fs::create_dir_all(safe_join(target, dir)?).map_err(|_| EngineError::Failed)?;
            }
        }
        for file in &manifest.files {
            if !selected(&file.path) {
                continue;
            }
            let path = safe_join(target, &file.path)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|_| EngineError::Failed)?;
            }
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|_| EngineError::Failed)?;
            for hash in &file.chunks {
                if !valid_hash(hash) {
                    return Err(EngineError::Corrupt);
                }
                let data = decompress(&self.store.get(&chunk_key(hash))?, CHUNK_SIZE)?;
                if hash_hex(&data) != *hash {
                    return Err(EngineError::Corrupt);
                }
                out.write_all(&data).map_err(|_| EngineError::Failed)?;
            }
            out.sync_all().map_err(|_| EngineError::Failed)?;
        }
        Ok(())
    }
}

/// Join a manifest path under `target`, refusing anything that could escape it
/// (repositories, especially shared buckets, are not trusted for paths).
fn safe_join(target: &Path, relative: &str) -> Result<PathBuf> {
    crate::files::RelativePath::parse(relative).map_err(|_| EngineError::Corrupt)?;
    Ok(native(target, relative))
}

fn native(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |p, s| p.join(s))
}

/// Directories and regular files under `root` (links and specials are skipped),
/// as `/`-separated relative paths with size and modification time.
#[allow(clippy::type_complexity)]
fn walk(root: &Path, ignore: &Ignore) -> Result<(Vec<String>, Vec<(String, u64, i64)>)> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(relative) = stack.pop() {
        let entries = fs::read_dir(native(root, &relative)).map_err(|_| EngineError::Incomplete)?;
        for entry in entries {
            let entry = entry.map_err(|_| EngineError::Incomplete)?;
            let name = entry
                .file_name()
                .to_str()
                .ok_or(EngineError::Incomplete)?
                .to_owned();
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let meta = entry
                .path()
                .symlink_metadata()
                .map_err(|_| EngineError::Incomplete)?;
            if meta.is_dir() {
                if !ignore.excluded(&path, true) {
                    dirs.push(path.clone());
                    stack.push(path);
                }
            } else if meta.is_file()
                && !ignore.excluded(&path, false)
                && !path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with(apply::TEMP_PREFIX))
            {
                if files.len() >= MAX_FILES {
                    return Err(EngineError::TooLarge);
                }
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
                files.push((path, meta.len(), modified));
            }
        }
    }
    dirs.sort();
    files.sort();
    Ok((dirs, files))
}

/// Which snapshots to keep under `retention` (each `(id, time_ms)`), like restic:
/// the newest N overall, plus the newest snapshot of each of the newest N days,
/// ISO weeks and months.
fn keep_set(snapshots: &[(String, i64)], retention: Retention) -> BTreeSet<String> {
    let mut newest_first: Vec<&(String, i64)> = snapshots.iter().collect();
    newest_first.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    let mut keep: BTreeSet<String> = newest_first
        .iter()
        .take(retention.keep_last as usize)
        .map(|(id, _)| id.clone())
        .collect();
    let mut bucketed = |count: u32, bucket: &dyn Fn(i64) -> i64| {
        let mut seen = BTreeSet::new();
        for (id, time) in &newest_first {
            if seen.len() >= count as usize {
                break;
            }
            if seen.insert(bucket(*time)) {
                keep.insert(id.clone());
            }
        }
    };
    let day = |ms: i64| ms.div_euclid(86_400_000);
    bucketed(retention.keep_daily, &day);
    // ISO weeks start on Monday; 1970-01-01 was a Thursday.
    bucketed(retention.keep_weekly, &|ms| (day(ms) + 3).div_euclid(7));
    bucketed(retention.keep_monthly, &|ms| {
        let (y, m, _) = civil(day(ms));
        y * 12 + i64::from(m)
    });
    keep
}

#[cfg(test)]
mod tests;
