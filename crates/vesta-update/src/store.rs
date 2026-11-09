use crate::{Metadata, Trust, metadata, release};
use fs2::FileExt;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};
use vesta_state::{StateDatabase, StateDirectory, SystemClock, artifacts::ArtifactName};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSource {
    pub local: Option<PathBuf>,
    pub base_url: Option<String>,
    pub asset_prefix: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StageStatus {
    pub sequence: u64,
    pub version: String,
    pub manifest_sha256: String,
    pub phase: String,
    pub directory: PathBuf,
}
pub struct UpdateStore {
    pub(super) db: StateDatabase,
}
pub(super) type Result<T> = std::result::Result<T, String>;
pub(super) fn unavailable<T>(_: T) -> String {
    "update_state_unavailable".into()
}
impl UpdateStore {
    pub fn open(root: &Path) -> Result<Self> {
        let directory = StateDirectory::new(root, None).map_err(unavailable)?;
        let db = StateDatabase::open(directory, Arc::new(SystemClock)).map_err(unavailable)?;
        db.write(|c|{c.execute_batch("CREATE TABLE IF NOT EXISTS update_trust(singleton INTEGER PRIMARY KEY CHECK(singleton=1),document TEXT NOT NULL CHECK(length(document)<=4096)) STRICT; CREATE TABLE IF NOT EXISTS update_stages(hash TEXT PRIMARY KEY,sequence INTEGER NOT NULL,version TEXT NOT NULL,phase TEXT NOT NULL CHECK(phase IN ('staging','staged')),metadata BLOB NOT NULL CHECK(length(metadata)<=16384),signature BLOB NOT NULL CHECK(length(signature)<=256)) STRICT;")?;Ok(())}).map_err(unavailable)?;
        db.write(|c| { c.execute_batch("CREATE TABLE IF NOT EXISTS update_manifest_proofs(hash TEXT PRIMARY KEY,document BLOB NOT NULL CHECK(length(document)<=16384),signature BLOB NOT NULL CHECK(length(signature)<=256)) STRICT;")?; Ok(()) }).map_err(unavailable)?;
        let store=Self { db };
        store.startup_schema()?;
        Ok(store)
    }
    pub(super) fn lock(&self) -> Result<File> {
        let directory = self.db.directory().artifacts().map_err(unavailable)?;
        let file = directory
            .open_or_create(&ArtifactName::new("update.lock").map_err(unavailable)?)
            .map_err(unavailable)?;
        file.try_lock_exclusive()
            .map_err(|_| "update_busy".to_owned())?;
        Ok(file)
    }
    /// Bootstrap from the exact already-installed manifest, not from network key material.
    pub fn initialize(
        &self,
        key: &str,
        channel: &str,
        installed_manifest: &str,
        source: &UpdateSource,
        now: i64,
    ) -> Result<Trust> {
        let _lock = self.lock()?;
        let mut proposed = Trust::new(key, channel, now)?;
        let (bytes, sig) = release::update_bytes(source)?;
        let (m, hash) = metadata::verify(&proposed, &bytes, &sig, now)?;
        if m.manifest_sha256 != installed_manifest {
            return Err("update_bootstrap_manifest_mismatch".into());
        }
        let release = release::load(release::Source {
            local: source.local.clone(),
            base_url: source.base_url.clone(),
            asset_prefix: source.asset_prefix.clone(),
            manifest_sha256: installed_manifest.into(),
            release_key: key.into(),
        })?;
        if release.manifest.version != m.version || release.manifest.target != m.target {
            return Err("update_manifest_mismatch".into());
        }
        proposed.sequence = m.sequence;
        proposed.metadata_sha256 = Some(hash);
        let old = self.trust_optional()?;
        if let Some(old) = old {
            if old.release_key != proposed.release_key
                || old.channel != proposed.channel
                || old.target != proposed.target
            {
                return Err("update_trust_already_pinned".into());
            }
            return Ok(old);
        }
        let json = serde_json::to_string(&proposed).map_err(unavailable)?;
        self.db
            .write(|c| {
                c.execute(
                    "INSERT INTO update_trust(singleton,document) VALUES(1,?)",
                    [json],
                )?;
                Ok(())
            })
            .map_err(unavailable)?;
        Ok(proposed)
    }
    fn trust_optional(&self) -> Result<Option<Trust>> {
        let value: Option<String> = self
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT document FROM update_trust WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .map_err(unavailable)?;
        value
            .map(|v| serde_json::from_str(&v).map_err(unavailable))
            .transpose()
    }
    pub fn trust(&self) -> Result<Trust> {
        self.trust_optional()?
            .ok_or("update_trust_not_initialized".into())
    }
    /// Verifies metadata without changing accepted sequence or downloading binaries.
    pub fn check(&self, source: &UpdateSource, now: i64) -> Result<Metadata> {
        let (bytes, sig) = release::update_bytes(source)?;
        metadata::verify(&self.trust()?, &bytes, &sig, now).map(|(m, _)| m)
    }
    pub fn status(&self) -> Result<Vec<StageStatus>> {
        let rows=self.db.read(|c|{
            let mut q=c.prepare("SELECT sequence,version,hash,phase FROM update_stages ORDER BY sequence DESC LIMIT 4")?;
            Ok(q.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).map_err(unavailable)?;
        rows.into_iter()
            .map(|(sequence, version, hash, phase)| {
                if !metadata::valid_hash(&hash) {
                    return Err("update_state_unavailable".into());
                }
                Ok(StageStatus {
                    sequence: u64::try_from(sequence).map_err(unavailable)?,
                    version,
                    directory: self.candidate(&hash),
                    manifest_sha256: hash,
                    phase,
                })
            })
            .collect()
    }
    pub(super) fn candidate(&self, hash: &str) -> PathBuf {
        self.db.directory().path().join("candidates").join(hash)
    }
    /// Persist the anti-rollback floor before downloading. Never changes active files.
    pub fn stage(
        &self,
        source: &UpdateSource,
        now: i64,
        progress: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<StageStatus> {
        let _lock = self.lock()?;
        let mut trust = self.trust()?;
        let (bytes, sig) = release::update_bytes(source)?;
        let (m, metadata_hash) = metadata::verify(&trust, &bytes, &sig, now)?;
        let verified = release::load(release::Source {
            local: source.local.clone(),
            base_url: source.base_url.clone(),
            asset_prefix: source.asset_prefix.clone(),
            manifest_sha256: m.manifest_sha256.clone(),
            release_key: trust.release_key.clone(),
        })?;
        if verified.manifest.version != m.version
            || verified.manifest.target != m.target
            || verified.manifest.files.len() != 4
        {
            return Err("update_manifest_mismatch".into());
        }
        let count: i64 = self
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM update_stages WHERE hash<>?",
                    [&m.manifest_sha256],
                    |r| r.get(0),
                )?)
            })
            .map_err(unavailable)?;
        if count >= 4 {
            return Err("update_staging_full".into());
        }
        let required = verified
            .manifest
            .files
            .iter()
            .try_fold(64 * 1024 * 1024u64, |n, f| n.checked_add(f.size))
            .ok_or("update_storage_low")?;
        if fs2::available_space(self.db.directory().path()).map_err(unavailable)? < required {
            return Err("update_storage_low".into());
        }
        trust.sequence = m.sequence;
        trust.metadata_sha256 = Some(metadata_hash);
        trust.last_seen_ms = now;
        let document = serde_json::to_string(&trust).map_err(unavailable)?;
        let sequence = i64::try_from(m.sequence).map_err(unavailable)?;
        let (manifest_bytes, manifest_signature) = verified.signed_manifest()?;
        self.db.write(|c| {
            c.execute("UPDATE update_trust SET document=? WHERE singleton=1",[&document])?;
            c.execute("INSERT INTO update_stages(hash,sequence,version,phase,metadata,signature) VALUES(?,?,?,'staging',?,?) ON CONFLICT(hash) DO UPDATE SET sequence=excluded.sequence,version=excluded.version,phase='staging',metadata=excluded.metadata,signature=excluded.signature",params![m.manifest_sha256,sequence,m.version,bytes,sig])?;
            c.execute("INSERT INTO update_manifest_proofs VALUES(?,?,?) ON CONFLICT(hash) DO UPDATE SET document=excluded.document,signature=excluded.signature",params![m.manifest_sha256,manifest_bytes,manifest_signature])?;
            Ok(())
        }).map_err(unavailable)?;
        let candidate = self.candidate(&m.manifest_sha256);
        StateDirectory::new(&candidate, None)
            .and_then(|d| d.prepare())
            .map_err(unavailable)?;
        verified.install(&candidate, progress)?;
        self.db
            .write(|c| {
                c.execute(
                    "UPDATE update_stages SET phase='staged' WHERE hash=?",
                    [&m.manifest_sha256],
                )?;
                Ok(())
            })
            .map_err(unavailable)?;
        Ok(StageStatus {
            sequence: m.sequence,
            version: m.version,
            manifest_sha256: m.manifest_sha256,
            phase: "staged".into(),
            directory: candidate,
        })
    }
}
