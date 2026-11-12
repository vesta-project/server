//! Content-addressed private pack files. Payload bytes never live in review JSON.
use crate::files::{FileError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, Weak},
};
use vesta_state::{
    StateDatabase,
    artifacts::{ArtifactDirectory, ArtifactName},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Payload {
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone)]
pub(crate) struct Store {
    db: StateDatabase,
    directory: ArtifactDirectory,
    gate: Arc<RwLock<()>>,
}
fn io_error(error: io::Error) -> FileError {
    if error.kind() == io::ErrorKind::StorageFull {
        FileError::InsufficientSpace
    } else {
        FileError::Unavailable
    }
}
fn name(payload: &Payload) -> Result<ArtifactName> {
    if payload.sha256.len() != 64
        || !payload
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(FileError::InvalidPath);
    }
    Ok(ArtifactName::new(&format!("blob-{}", payload.sha256))?)
}
impl Store {
    pub fn upload_create(&self) -> Result<String> {
        crate::files::require_space(self.db.directory().path(), 0)?;
        let name = ArtifactName::new(&format!("upload-{}", uuid::Uuid::new_v4().simple()))?;
        self.directory
            .create_new(&name)?
            .sync_all()
            .map_err(io_error)?;
        self.directory.sync()?;
        Ok(name.as_str().to_owned())
    }
    pub fn upload_open(&self, name: &str) -> Result<File> {
        if !name.starts_with("upload-") {
            return Err(FileError::InvalidPath);
        }
        self.directory
            .open(&ArtifactName::new(name)?)?
            .ok_or(FileError::NotFound)
    }
    pub fn upload_append(&self, name: &str, offset: u64, bytes: &[u8]) -> Result<()> {
        crate::files::require_space(self.db.directory().path(), bytes.len() as u64)?;
        let mut file = self.upload_open(name)?;
        if file.metadata().map_err(io_error)?.len() < offset {
            return Err(FileError::Corrupt);
        }
        // A failed SQL commit may leave an unacknowledged tail. The durable offset wins.
        file.set_len(offset).map_err(io_error)?;
        file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)
    }
    pub fn open(db: &StateDatabase) -> Result<Self> {
        static GATES: OnceLock<Mutex<HashMap<PathBuf, Weak<RwLock<()>>>>> = OnceLock::new();
        let mut gates = GATES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| FileError::Unavailable)?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        let key = db.directory().path().to_owned();
        let gate = gates.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
            let gate = Arc::new(RwLock::new(()));
            gates.insert(key, Arc::downgrade(&gate));
            gate
        });
        Ok(Self {
            db: db.clone(),
            directory: db.directory().pack_payloads()?,
            gate,
        })
    }
    pub fn cleanup(&self) -> Cleanup {
        Cleanup(self.clone())
    }
    pub fn lease(&self) -> Result<RwLockReadGuard<'_, ()>> {
        self.gate.read().map_err(|_| FileError::Unavailable)
    }
    pub fn stage(&self) -> Result<Stage> {
        crate::files::require_space(self.db.directory().path(), 64 * 1024)?;
        let temporary = ArtifactName::new(&format!("stage-{}", uuid::Uuid::new_v4().simple()))?;
        let file = self.directory.create_new(&temporary)?;
        Ok(Stage {
            store: self.clone(),
            temporary,
            file: Some(file),
            hash: Sha256::new(),
            size: 0,
            checked_at: 0,
        })
    }
    pub fn put(&self, reader: &mut impl Read) -> Result<Payload> {
        let mut stage = self.stage()?;
        io::copy(reader, &mut stage).map_err(io_error)?;
        stage.finish()
    }
    pub fn open_file(&self, payload: &Payload) -> Result<File> {
        let file = self
            .directory
            .open(&name(payload)?)?
            .ok_or(FileError::NotFound)?;
        if file.metadata().map_err(io_error)?.len() != payload.size {
            return Err(FileError::Corrupt);
        }
        Ok(file)
    }
    pub fn verify(&self, payload: &Payload) -> Result<File> {
        let mut file = self.open_file(payload)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(io_error)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if hex::encode(hash.finalize()) != payload.sha256 {
            return Err(FileError::Corrupt);
        }
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        Ok(file)
    }
    /// Collection never races a review, upload or pending effect holding a lease.
    pub fn collect(&self) -> Result<()> {
        let Ok(_exclusive) = self.gate.try_write() else {
            return Ok(());
        };
        let keep = self.db.read(|c| {
            let mut keep = BTreeSet::new();
            let exists = |table: &str| c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?)", [table], |r| r.get::<_, bool>(0));
            if exists("pack_install_plans")? {
                let mut q = c.prepare("SELECT json_extract(value,'$.sha256') FROM pack_install_plans,json_each(body,'$.payloads') WHERE expires>=?")?;
                for row in q.query_map([self.db.clock().millis()], |r| r.get::<_, String>(0))? { keep.insert(format!("blob-{}", row?)); }
            }
            if exists("pack_install_plans")? {
                for field in ["bases", "client_blobs"] {
                    let mut q=c.prepare(&format!("SELECT json_extract(value,'$.sha256') FROM pack_install_plans,json_each(body,'$.{field}') WHERE expires>=?"))?;
                    for row in q.query_map([self.db.clock().millis()], |r|r.get::<_,String>(0))? {keep.insert(format!("blob-{}",row?));}
                }
            }
            if exists("server_modpack_bases")? {
                let mut q = c.prepare("SELECT json_extract(payload,'$.sha256') FROM server_modpack_bases")?;
                for row in q.query_map([], |r| r.get::<_, String>(0))? { keep.insert(format!("blob-{}", row?)); }
            }
            if exists("server_content_changes")? {
                let mut q = c.prepare("SELECT json_extract(change_json,'$.payload.sha256') FROM server_content_changes WHERE state='pending' AND json_extract(change_json,'$.payload.sha256') IS NOT NULL")?;
                for row in q.query_map([], |r| r.get::<_, String>(0))? { keep.insert(format!("blob-{}", row?)); }
            }
            if exists("pack_spooled_uploads")? {
                let mut q = c.prepare("SELECT CASE WHEN u.complete=0 THEN s.name END,s.payload FROM pack_spooled_uploads AS s JOIN pack_workspace_uploads AS u USING(server_id,principal_id,device_id,upload_id)")?;
                for row in q.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?,r.get::<_, Option<String>>(1)?)))? {
                    let (name,payload) = row?; if let Some(name)=name {keep.insert(name);}
                    if let Some(payload)=payload { let p:Payload=serde_json::from_str(&payload).map_err(|_|vesta_state::StateError::new("state_pack_payload_invalid"))?;keep.insert(format!("blob-{}",p.sha256)); }
                }
            }
            Ok(keep)
        })?;
        for entry in self.directory.list(usize::MAX)? {
            if (entry.as_str().starts_with("blob-")
                || entry.as_str().starts_with("stage-")
                || entry.as_str().starts_with("upload-"))
                && !keep.contains(entry.as_str())
            {
                self.directory.remove(&entry)?;
            }
        }
        Ok(())
    }
}
pub(crate) struct Stage {
    store: Store,
    temporary: ArtifactName,
    file: Option<File>,
    hash: Sha256,
    size: u64,
    checked_at: u64,
}
impl Write for Stage {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.size == 0 || self.size.saturating_sub(self.checked_at) >= 8 * 1024 * 1024 {
            crate::files::require_space(
                self.store.db.directory().path(),
                (bytes.len() as u64).max(8 * 1024 * 1024),
            )
            .map_err(|e| io::Error::new(io::ErrorKind::StorageFull, e.code()))?;
            self.checked_at = self.size;
        }
        let count = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("closed pack stage"))?
            .write(bytes)?;
        self.hash.update(&bytes[..count]);
        self.size = self
            .size
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("pack file size overflow"))?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("closed pack stage"))?
            .flush()
    }
}
impl Stage {
    pub fn finish(mut self) -> Result<Payload> {
        self.file
            .take()
            .ok_or(FileError::Unavailable)?
            .sync_all()
            .map_err(io_error)?;
        let payload = Payload {
            sha256: hex::encode(self.hash.clone().finalize()),
            size: self.size,
        };
        let target = name(&payload)?;
        match self.store.directory.link_new(&self.temporary, &target) {
            Ok(()) => {
                self.store.directory.remove(&self.temporary)?;
            }
            Err(e) if e.code() == "state_artifact_exists" => {
                self.store.verify(&payload)?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(payload)
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        self.file.take();
        let _ = self.store.directory.remove(&self.temporary);
    }
}

pub(crate) struct Cleanup(Store);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.collect();
    }
}
