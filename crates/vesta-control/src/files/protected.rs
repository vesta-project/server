//! Explicit, confirmed raw access to Minecraft admission documents.
use super::*;
use crate::operations::{self, ConfirmationToken, PropertiesApproval};
use serde_json::{Value, json};

pub fn protected_minecraft_file(path: &str) -> bool {
    [
        "server.properties",
        "whitelist.json",
        "ops.json",
        "banned-players.json",
        "banned-ips.json",
    ]
    .iter()
    .any(|p| path.eq_ignore_ascii_case(p))
}
impl FileBrowser {
    fn raw_protected(&self) -> Self {
        let mut files = self.clone();
        files.root.remote_documents = false;
        files
    }
    pub fn list_editable(
        &self,
        subject: &Subject,
        path: Option<&RelativePath>,
    ) -> Result<DirectoryListing> {
        if self
            .check_global_authority(subject, Capability::ConfigEdit)
            .is_ok()
        {
            self.raw_protected().list(subject, path)
        } else {
            self.list(subject, path)
        }
    }
    pub fn read_editable(
        &self,
        subject: &Subject,
        path: &RelativePath,
        limit: usize,
    ) -> Result<FileRead> {
        if !protected_minecraft_file(path.as_str()) {
            return self.read(subject, path, limit);
        }
        let read = self.raw_protected().read_with_capability(
            subject,
            path,
            limit,
            Some(Capability::ConfigEdit),
        )?;
        self.audit
            .append_record(&vesta_state::audit::AuditRecord::of(
                vesta_state::audit::AuditActor::new(
                    subject.principal.as_str().into(),
                    subject.device.as_str().into(),
                    "files".into(),
                )?,
                "files.protected.read".into(),
                path.as_str().into(),
                "success".into(),
                None,
            )?)?;
        Ok(read)
    }
    #[allow(clippy::too_many_arguments)] // Exact file payload, validator, receipt and durable intent.
    pub fn write_protected(
        &self,
        subject: &Subject,
        path: &RelativePath,
        expected: &str,
        bytes: &[u8],
        token: Option<&str>,
        key: &str,
    ) -> Result<Value> {
        if !protected_minecraft_file(path.as_str()) {
            return Err(FileError::ProtectedPath);
        }
        self.check_global_authority(subject, Capability::ConfigEdit)?;
        if path.as_str().eq_ignore_ascii_case("server.properties") {
            crate::config::validate_raw_properties(bytes)?;
        } else {
            let value: Value = serde_json::from_slice(bytes).map_err(|_| FileError::InvalidPath)?;
            if !value.is_array() {
                return Err(FileError::InvalidPath);
            }
        }
        let input = json!([
            "raw-protected",
            path.as_str().to_ascii_lowercase(),
            expected,
            hex::encode(Sha256::digest(bytes))
        ]);
        let payload_hash = hex::encode(Sha256::digest(input.to_string().as_bytes()));
        let mut files = self.raw_protected();
        // Replay before token checks: a lost success reply is recoverable without a second effect.
        if let Some(value) = files.remote_replay_before_preparation(subject, key, &input)? {
            let mut value = value;
            if let Some(object) = value.as_object_mut() {
                object.remove("outcome");
                object.insert("state".into(), json!("applied"));
                object.insert("restart_needed".into(), json!(true));
            }
            return Ok(value);
        }
        let read = files.read_with_capability(
            subject,
            path,
            MAX_READ_BYTES,
            Some(Capability::ConfigEdit),
        )?;
        if read.sha256() != expected {
            return Err(FileError::Conflict(Some(read.sha256().into())));
        }
        let Some(token) = token else {
            let receipt = operations::prepare_properties_confirmation(
                &self.db,
                subject,
                &payload_hash,
                expected,
                key,
            )?;
            return Ok(
                json!({"state":"awaiting_confirmation","operation_id":receipt.operation_id.as_str(),"confirmation_token":receipt.token.expose_secret(),"expires_at_ms":receipt.expires_at_ms}),
            );
        };
        let approval = PropertiesApproval {
            token_hash: ConfirmationToken::try_from(token.to_owned())
                .map_err(|_| FileError::ConfirmationInvalid)?
                .hash()
                .to_string(),
            payload_hash,
            expected_sha256: expected.into(),
        };
        files.properties_approval = Some(approval);
        files.execute_remote(subject,key,input,|| Ok(vec![(Capability::ConfigEdit.id().into(),String::new()),(Capability::FilesOverwrite.id().into(),path.as_str().into())]),|files| {
            let change = files.write(subject,path,&WritePrecondition::Matches(expected.into()),bytes)?;
            Ok(json!({"state":"applied","sha256":change.sha256,"size":change.size.to_string(),"trash_id":change.trash,"restart_needed":true}))
        })
    }
}
