use super::*;
use crate::packs::payload::{Payload, Store};
fn storage(error: crate::files::FileError) -> ReleaseError {
    super::super::PackError::from(error).into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UploadBegin {
    pub upload_id: String,
    pub total_size: String,
    pub sha256: String,
    pub archive: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UploadChunk {
    pub upload_id: String,
    pub offset: String,
    pub content_base64: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UploadReceipt {
    pub upload_id: String,
    pub size: String,
    pub sha256: String,
    pub complete: bool,
}
impl PackWorkspace {
    pub fn upload_begin(
        &self,
        subject: &Subject,
        key: &str,
        input: &UploadBegin,
    ) -> Result<UploadReceipt> {
        let store = Store::open(&self.db).map_err(storage)?;
        let _cleanup = store.cleanup();
        let _lease = store.lease().map_err(storage)?;
        self.mutate(subject,key,input,|c|{
            let size=revision(&input.total_size)?;
            if !identifier(&input.upload_id)||!valid_sha(&input.sha256)||size>i64::MAX as u64||(!input.archive&&size>super::super::MAX_PINNED_FILE_BYTES as u64){return Err(invalid());}
            let count:i64=c.query_row("SELECT count(*) FROM pack_workspace_uploads",[],|r|r.get(0))?;
            if count>=8{return Err(super::super::PackError::TooLarge.into());}
            c.execute("INSERT INTO pack_workspace_uploads(server_id,principal_id,device_id,upload_id,total_size,sha256,archive,body) VALUES(?,?,?,?,?,?,?,X'')",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id,size as i64,input.sha256,input.archive])?;
            let name=store.upload_create().map_err(storage)?;
            c.execute("INSERT OR REPLACE INTO pack_spooled_uploads(server_id,principal_id,device_id,upload_id,name,received,payload) VALUES(?,?,?,?,?,0,NULL)",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id,name])?;
            Ok(UploadReceipt{upload_id:input.upload_id.clone(),size:"0".into(),sha256:input.sha256.clone(),complete:false})
        })
    }
    pub fn upload_chunk(
        &self,
        subject: &Subject,
        key: &str,
        input: &UploadChunk,
    ) -> Result<UploadReceipt> {
        self.upload_chunk_with(subject,key,input,|store,name,offset,bytes| store.upload_append(name,offset,bytes).map_err(storage))
    }
    fn upload_chunk_with(&self, subject: &Subject, key: &str, input: &UploadChunk, append: impl FnOnce(&Store,&str,u64,&[u8])->Result<()>) -> Result<UploadReceipt> {
        // Serialize spool offset changes, independently of the database writer.
        static APPEND: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _append=APPEND.lock().map_err(|_|invalid())?;
        if let Some(receipt)=self.replay(subject,key,input)? { return Ok(receipt); }
        let bytes=STANDARD.decode(&input.content_base64).map_err(|_|invalid())?;
        if bytes.is_empty() || bytes.len()>512*1024 { return Err(invalid()); }
        let store=Store::open(&self.db).map_err(storage)?;
        let _lease=store.lease().map_err(storage)?;
        let (size,total,sha,complete,name):(i64,i64,String,bool,Option<String>)=self.db.read_with(|t| {
            let c=t.connection(); self.authorize_in(c,subject,Capability::PackEdit)?;
            Ok::<_,ReleaseError>(c.query_row("SELECT coalesce(s.received,length(u.body)),u.total_size,u.sha256,u.complete,s.name FROM pack_workspace_uploads u LEFT JOIN pack_spooled_uploads s USING(server_id,principal_id,device_id,upload_id) WHERE u.server_id=? AND u.principal_id=? AND u.device_id=? AND u.upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?)
        })?;
        if complete || revision(&input.offset)?!=size as u64 || size+bytes.len() as i64>total { return Err(ReleaseError::Conflict); }
        if let Some(name)=&name { append(&store,name,size as u64,&bytes)?; }
        self.mutate(subject,key,input,|c| {
            if let Some(name)=&name {
                let count=c.execute("UPDATE pack_spooled_uploads SET received=? WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=? AND name=? AND received=? AND EXISTS(SELECT 1 FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=? AND complete=0)",params![size+bytes.len() as i64,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id,name,size,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id])?;
                if count!=1 { return Err(ReleaseError::Conflict); }
            } else {
                let count=c.execute("UPDATE pack_workspace_uploads SET body=CAST(body||? AS BLOB) WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=? AND length(body)=? AND complete=0",params![bytes,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id,size])?;
                if count!=1 { return Err(ReleaseError::Conflict); }
            }
            c.execute("INSERT OR IGNORE INTO pack_upload_chunk_receipts VALUES(?,?,?,?)",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key])?;
            Ok(UploadReceipt{upload_id:input.upload_id.clone(),size:(size+bytes.len() as i64).to_string(),sha256:sha,complete:false})
        })
    }
    pub fn upload_finish(&self, subject: &Subject, key: &str, id: &str) -> Result<UploadReceipt> {
        if let Some(receipt) = self.replay(subject, key, &("finish", id))? {
            return Ok(receipt);
        }
        let store = Store::open(&self.db).map_err(storage)?;
        let _cleanup = store.cleanup();
        let _lease = store.lease().map_err(storage)?;
        let (total,sha,archive,legacy,spooled)=self.db.read_with(|tx| {
            let c=tx.connection();self.authorize_in(c,subject,Capability::PackEdit)?;
            let row:(i64,String,bool,Vec<u8>)=c.query_row("SELECT total_size,sha256,archive,body FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let spool:Option<(String,i64,Option<String>)>=c.query_row("SELECT name,received,payload FROM pack_spooled_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            Ok::<_,ReleaseError>((row.0,row.1,row.2,row.3,spool))
        })?;
        // Copying and hashing a large archive must never hold the database writer.
        let payload = if let Some((name, received, existing)) = &spooled {
            if *received != total {
                return Err(ReleaseError::Conflict);
            }
            if let Some(json) = existing {
                let payload: Payload = serde_json::from_str(json).map_err(|_| invalid())?;
                store.verify(&payload).map_err(storage)?;
                payload
            } else {
                let mut file = store.upload_open(name).map_err(storage)?;
                if file.metadata().map_err(|_| invalid())?.len() != total as u64 {
                    return Err(ReleaseError::Conflict);
                }
                store.put(&mut file).map_err(storage)?
            }
        } else {
            store
                .put(&mut std::io::Cursor::new(&legacy))
                .map_err(storage)?
        };
        if payload.sha256 != sha || payload.size != total as u64 {
            return Err(ReleaseError::Conflict);
        }
        let body = if !archive {
            let mut body = Vec::new();
            use std::io::Read;
            store
                .verify(&payload)
                .map_err(storage)?
                .take(super::super::MAX_PINNED_FILE_BYTES as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|_| invalid())?;
            Some(body)
        } else {
            None
        };
        self.mutate(subject,key,&("finish",id),|c|{
            let current:(i64,String)=c.query_row("SELECT total_size,sha256 FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if current!=(total,sha.clone()) {return Err(ReleaseError::Conflict);}
            if let Some((name,received,_))=&spooled {
                let updated=c.execute("UPDATE pack_spooled_uploads SET payload=? WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=? AND name=? AND received=?",params![serde_json::to_string(&payload).map_err(|_|invalid())?,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id,name,received])?;
                if updated!=1 {return Err(ReleaseError::Conflict);}
            }
            if let Some(body)=body {self.store_blob(c,&body)?;}
            c.execute("UPDATE pack_workspace_uploads SET complete=1 WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id])?;
            if !archive {c.execute("DELETE FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id])?;}
            Ok(UploadReceipt{upload_id:id.into(),size:total.to_string(),sha256:sha,complete:true})
        })
    }
    pub fn upload_cancel(&self, subject: &Subject, key: &str, id: &str) -> Result<bool> {
        let result=self.mutate(subject,key,&("cancel",id),|c|Ok(c.execute("DELETE FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id])?>0));
        let _ = Store::open(&self.db).and_then(|store| store.collect());
        result
    }
    pub(super) fn archive_payload(
        &self,
        subject: &Subject,
        id: &str,
        store: &Store,
    ) -> Result<Payload> {
        self.db.read_with(|s|{
            self.authorize_in(s.connection(),subject,Capability::PackEdit)?;
            let (payload,body):(Option<String>,Vec<u8>)=s.connection().query_row("SELECT s.payload,u.body FROM pack_workspace_uploads AS u LEFT JOIN pack_spooled_uploads AS s USING(server_id,principal_id,device_id,upload_id) WHERE u.server_id=? AND u.principal_id=? AND u.device_id=? AND u.upload_id=? AND u.archive=1 AND u.complete=1",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if let Some(json)=payload {serde_json::from_str(&json).map_err(|_|invalid())} else {store.put(&mut std::io::Cursor::new(body)).map_err(storage)}
        }).map_err(Into::into)
    }
    pub(super) fn archive_upload(&self, subject: &Subject, id: &str) -> Result<Vec<u8>> {
        let store = Store::open(&self.db).map_err(storage)?;
        let _cleanup = store.cleanup();
        let _lease = store.lease().map_err(storage)?;
        let payload = self.archive_payload(subject, id, &store)?;
        let mut bytes = Vec::new();
        use std::io::Read;
        store
            .verify(&payload)
            .map_err(storage)?
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod writer_regression {
    use super::*;
    #[test]
    fn spool_sync_does_not_own_database_writer_and_replay_does_not_append() {
        let f=super::super::tests::Fixture::new();
        f.workspace.upload_begin(&f.subject,"begin",&UploadBegin {upload_id:"spool-test".into(),total_size:"3".into(),sha256:digest(b"abc"),archive:true}).unwrap();
        let input=UploadChunk {upload_id:"spool-test".into(),offset:"0".into(),content_base64:STANDARD.encode(b"abc")};
        let receipt=f.workspace.upload_chunk_with(&f.subject,"chunk",&input,|store,name,offset,bytes| {
            // A nested writer acquisition would fail (or deadlock) if append held it.
            f.db.write(|c| {c.execute_batch("CREATE TABLE upload_writer_probe(value INTEGER)")?; Ok(())}).unwrap();
            store.upload_append(name,offset,bytes).map_err(storage)
        }).unwrap();
        assert_eq!(receipt.size,"3");
        let replay=f.workspace.upload_chunk_with(&f.subject,"chunk",&input,|_,_,_,_|panic!("replay appended twice")).unwrap();
        assert_eq!(replay.size,"3");
        assert!(f.workspace.upload_finish(&f.subject,"finish","spool-test").unwrap().complete);
    }
}
