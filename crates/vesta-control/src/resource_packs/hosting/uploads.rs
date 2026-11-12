use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::Write,
    time::{Duration, Instant},
};

pub const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
pub const INPUT_BYTES: u64 = 64 * 1024 * 1024;
const STORED_BYTES: i64 = 256 * 1024 * 1024;
pub(super) fn blob_name(sha: &str) -> Result<ArtifactName> {
    if !valid_sha(sha) {
        return Err(Error::Invalid);
    }
    ArtifactName::new(&format!("rp-input-{sha}.zip")).map_err(Into::into)
}
pub(super) fn stored(c: &Connection) -> Result<i64> {
    Ok(c.query_row("SELECT (SELECT coalesce(sum(size),0) FROM hosted_pack_blobs)+(SELECT coalesce(sum(size),0) FROM hosted_pack_uploads)",[],|r|r.get(0))?)
}
pub(super) fn store_bytes(inner: &Inner, bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() || bytes.len() as u64 > INPUT_BYTES {
        return Err(Error::Limit);
    }
    let sha = hash(bytes);
    let _lock = inner.admission.lock().map_err(|_| Error::Unavailable)?;
    let exists: bool = inner.db.read(|c| {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM hosted_pack_blobs WHERE sha256=?)",
            [&sha],
            |r| r.get(0),
        )?)
    })?;
    if exists {
        return Ok(sha);
    }
    let total = inner.db.read_with(|tx| stored(tx.connection()))?;
    if total + bytes.len() as i64 > STORED_BYTES {
        return Err(Error::Limit);
    }
    let dir = inner.db.directory().uploads()?;
    let name = blob_name(&sha)?;
    write_new_or_verify(&dir, &name, bytes, &sha)?;
    inner.db.write(|c| {
        c.execute(
            "INSERT OR IGNORE INTO hosted_pack_blobs VALUES(?,?)",
            params![sha, bytes.len() as i64],
        )?;
        Ok(())
    })?;
    Ok(sha)
}
fn write_new_or_verify(
    dir: &ArtifactDirectory,
    name: &ArtifactName,
    bytes: &[u8],
    sha: &str,
) -> Result<()> {
    match dir.create_new(name) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                drop(file);
                let _ = dir.remove(name);
                return Err(e.into());
            }
            drop(file);
            dir.sync()?;
        }
        Err(e) if e.code() == "state_artifact_exists" => {
            let mut file = dir.open(name)?.ok_or(Error::Unavailable)?;
            let (actual, _, size) = super::super::archive::hashes(
                &mut file,
                INPUT_BYTES,
                Instant::now() + Duration::from_secs(120),
            )?;
            if actual != sha || size != bytes.len() as u64 {
                return Err(Error::Build(super::super::Error::ChangedInput));
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
impl Handle {
    pub fn upload_begin(&self, actor: &Subject, key: &str, input: &UploadBegin) -> Result<Upload> {
        let size = revision(&input.size)?;
        if size == 0 || size as u64 > INPUT_BYTES || !valid_sha(&input.sha256) {
            return Err(Error::Invalid);
        }
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        self.mutate(actor,key,&("upload-begin",input),|c|{
            let count:i64=c.query_row("SELECT count(*) FROM hosted_pack_uploads",[],|r|r.get(0))?;
            if count>=4||stored(c)?+size>STORED_BYTES{return Err(Error::Limit);}
            let id=id();c.execute("INSERT INTO hosted_pack_uploads(id,principal,device,size,sha256) VALUES(?,?,?,?,?)",params![id,actor.principal.as_str(),actor.device.as_str(),size,input.sha256])?;
            Ok(Upload{id,size:input.size.clone(),received:"0".into(),sha256:input.sha256.clone()})
        })
    }
    fn upload_in(&self, c: &Connection, actor: &Subject, id: &str) -> Result<Upload> {
        let row:Option<(i64,i64,String)>=c.query_row("SELECT size,received,sha256 FROM hosted_pack_uploads WHERE id=? AND principal=? AND device=?",params![id,actor.principal.as_str(),actor.device.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (size, received, sha256) = row.ok_or(Error::NotFound)?;
        Ok(Upload {
            id: id.into(),
            size: size.to_string(),
            received: received.to_string(),
            sha256,
        })
    }
    pub fn upload_chunk(&self, actor: &Subject, key: &str, input: &UploadChunk) -> Result<Upload> {
        if input.content_base64.len() > UPLOAD_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(Error::Limit);
        }
        let bytes = STANDARD
            .decode(&input.content_base64)
            .map_err(|_| Error::Invalid)?;
        if bytes.is_empty() || bytes.len() > UPLOAD_CHUNK_BYTES {
            return Err(Error::Invalid);
        }
        let offset = revision(&input.offset)?;
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        self.mutate(
            actor,
            key,
            &("upload-chunk", &input.upload_id, offset, hash(&bytes)),
            |c| {
                let mut upload = self.upload_in(c, actor, &input.upload_id)?;
                let received = revision(&upload.received)?;
                if offset != received || received + bytes.len() as i64 > revision(&upload.size)? {
                    return Err(Error::Conflict);
                }
                let count: i64 = c.query_row(
                    "SELECT count(*) FROM hosted_pack_chunks WHERE upload_id=?",
                    [&input.upload_id],
                    |r| r.get(0),
                )?;
                if count >= 1024 {
                    return Err(Error::Limit);
                }
                c.execute(
                    "INSERT INTO hosted_pack_chunks VALUES(?,?,?)",
                    params![input.upload_id, offset, bytes],
                )?;
                c.execute(
                    "UPDATE hosted_pack_uploads SET received=received+? WHERE id=?",
                    params![bytes.len() as i64, input.upload_id],
                )?;
                upload.received = (received + bytes.len() as i64).to_string();
                Ok(upload)
            },
        )
    }
    pub fn upload_cancel(&self, actor: &Subject, key: &str, id: &str) -> Result<bool> {
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        self.mutate(actor, key, &("upload-cancel", id), |c| {
            self.upload_in(c, actor, id)?;
            c.execute("DELETE FROM hosted_pack_uploads WHERE id=?", [id])?;
            Ok(true)
        })
    }
    pub fn upload_finish(&self, actor: &Subject, key: &str, id: &str) -> Result<Upload> {
        self.authorize(actor)?;
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        // Durable receipt handles retry after chunks have already been retired.
        if let Some(upload) = self.upload_replay(actor, key, id)? {
            return Ok(upload);
        }
        let upload = self
            .inner
            .db
            .read_with(|tx| self.upload_in(tx.connection(), actor, id))?;
        if upload.received != upload.size {
            return Err(Error::Conflict);
        }
        let dir = self.inner.db.directory().uploads()?;
        if dir.count(4095)?.is_none() {
            return Err(Error::Limit);
        }
        let stage = ArtifactName::new(&format!("rp-stage-{}", self::id()))?;
        let result = (|| -> Result<()> {
            let mut output = dir.create_new(&stage)?;
            let mut digest = Sha256::new();
            let mut offset = 0i64;
            while offset < revision(&upload.size)? {
                let bytes: Vec<u8> = self.inner.db.read(|c| {
                    Ok(c.query_row(
                        "SELECT body FROM hosted_pack_chunks WHERE upload_id=? AND offset=?",
                        params![id, offset],
                        |r| r.get(0),
                    )?)
                })?;
                if bytes.is_empty() || bytes.len() > UPLOAD_CHUNK_BYTES {
                    return Err(Error::Unavailable);
                }
                output.write_all(&bytes)?;
                digest.update(&bytes);
                offset += bytes.len() as i64;
            }
            if offset != revision(&upload.size)? || hex::encode(digest.finalize()) != upload.sha256
            {
                return Err(Error::Conflict);
            }
            output.sync_all()?;
            drop(output);
            dir.sync()?;
            let name = blob_name(&upload.sha256)?;
            match dir.link_new(&stage, &name) {
                Ok(()) => (),
                Err(e) if e.code() == "state_artifact_exists" => {
                    let mut file = dir.open(&name)?.ok_or(Error::Unavailable)?;
                    let (sha, _, size) = super::super::archive::hashes(
                        &mut file,
                        INPUT_BYTES,
                        Instant::now() + Duration::from_secs(120),
                    )?;
                    if sha != upload.sha256 || size.to_string() != upload.size {
                        return Err(Error::Conflict);
                    }
                }
                Err(e) => return Err(e.into()),
            }
            Ok(())
        })();
        let _ = dir.remove(&stage);
        result?;
        self.mutate(actor, key, &("upload-finish", id), |c| {
            self.upload_in(c, actor, id)?;
            c.execute(
                "INSERT OR IGNORE INTO hosted_pack_blobs VALUES(?,?)",
                params![upload.sha256, revision(&upload.size)?],
            )?;
            c.execute("DELETE FROM hosted_pack_uploads WHERE id=?", [id])?;
            Ok(upload)
        })
    }
    fn upload_replay(&self, actor: &Subject, key: &str, id: &str) -> Result<Option<Upload>> {
        self.replay(actor, key, &("upload-finish", id))
    }
}
