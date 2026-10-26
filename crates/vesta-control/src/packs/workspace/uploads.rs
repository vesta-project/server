use super::*;
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
        self.mutate(subject,key,input,|c|{
            let size=revision(&input.total_size)?;
            if !identifier(&input.upload_id)||!valid_sha(&input.sha256)||size>super::super::MAX_ARTIFACT_BYTES as u64||(!input.archive&&size>super::super::MAX_PINNED_FILE_BYTES as u64){return Err(invalid());}
            let (count,total):(i64,i64)=c.query_row("SELECT count(*),coalesce(sum(total_size),0) FROM pack_workspace_uploads",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if count>=8||total+size as i64>128*1024*1024{return Err(super::super::PackError::TooLarge.into());}
            c.execute("INSERT INTO pack_workspace_uploads(server_id,principal_id,device_id,upload_id,total_size,sha256,archive,body) VALUES(?,?,?,?,?,?,?,X'')",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id,size as i64,input.sha256,input.archive])?;
            Ok(UploadReceipt{upload_id:input.upload_id.clone(),size:"0".into(),sha256:input.sha256.clone(),complete:false})
        })
    }
    pub fn upload_chunk(
        &self,
        subject: &Subject,
        key: &str,
        input: &UploadChunk,
    ) -> Result<UploadReceipt> {
        self.mutate(subject,key,input,|c|{
            let bytes=STANDARD.decode(&input.content_base64).map_err(|_|invalid())?;
            if bytes.is_empty()||bytes.len()>32768{return Err(invalid());}
            let (size,total,sha,complete):(i64,i64,String,bool)=c.query_row("SELECT length(body),total_size,sha256,complete FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            if complete||revision(&input.offset)?!=size as u64||size+bytes.len() as i64>total{return Err(ReleaseError::Conflict);}
            c.execute("UPDATE pack_workspace_uploads SET body=CAST(body||? AS BLOB) WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![bytes,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),input.upload_id])?;
            Ok(UploadReceipt{upload_id:input.upload_id.clone(),size:(size+bytes.len() as i64).to_string(),sha256:sha,complete:false})
        })
    }
    pub fn upload_finish(&self, subject: &Subject, key: &str, id: &str) -> Result<UploadReceipt> {
        self.mutate(subject,key,&("finish",id),|c|{
            let (total,sha,archive,bytes):(i64,String,bool,Vec<u8>)=c.query_row("SELECT total_size,sha256,archive,body FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            if bytes.len() as i64!=total||digest(&bytes)!=sha{return Err(ReleaseError::Conflict);}
            if !archive {self.store_blob(c,&bytes)?;}
            c.execute("UPDATE pack_workspace_uploads SET complete=1 WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id])?;
            if !archive {c.execute("DELETE FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id])?;}
            Ok(UploadReceipt{upload_id:id.into(),size:total.to_string(),sha256:sha,complete:true})
        })
    }
    pub fn upload_cancel(&self, subject: &Subject, key: &str, id: &str) -> Result<bool> {
        self.mutate(subject,key,&("cancel",id),|c|Ok(c.execute("DELETE FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id])?>0))
    }
    pub(super) fn archive_upload(&self, subject: &Subject, id: &str) -> Result<Vec<u8>> {
        Ok(self.db.read_with(|s|{self.authorize_in(s.connection(),subject,Capability::PackEdit)?;Ok(s.connection().query_row("SELECT body FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=? AND archive=1 AND complete=1",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),id],|r|r.get(0))?)})?)
    }
}
