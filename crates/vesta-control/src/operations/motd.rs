//! Reserved, validated presentation settings; no file or console authority.
use super::*;
use crate::motd::Document;
use rusqlite::OptionalExtension;
use serde::Serialize;

pub const KEY: &str = "motd-policy";
pub const MAX_POLICY_BYTES: usize = 32_768;
#[derive(Serialize)]
pub struct MotdPolicy {
    pub revision: Option<i64>,
    pub document: Document,
    pub live_apply: &'static str,
}
pub(crate) fn effective(c: &rusqlite::Connection, server: &str) -> vesta_state::Result<MotdPolicy> {
    let stored: Option<(i64, String)> = c
        .query_row(
            "SELECT desired_revision,desired_value FROM settings WHERE server_id=? AND key=?",
            rusqlite::params![server, KEY],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (revision, document) = match stored {
        None => (None, Document::default()),
        Some((revision, json)) => {
            if json.len() > MAX_POLICY_BYTES {
                return Err(vesta_state::StateError::new("state_invalid_request"));
            }
            let document: Document = serde_json::from_str(&json)
                .map_err(|_| vesta_state::StateError::new("state_invalid_request"))?;
            if !document.valid() {
                return Err(vesta_state::StateError::new("state_invalid_request"));
            }
            (Some(revision), document)
        }
    };
    Ok(MotdPolicy {
        revision,
        document,
        live_apply: "separate_server_properties_edit_required",
    })
}
impl Operations {
    pub fn motd_policy(&self, subject: &Subject) -> OperationsResult<MotdPolicy> {
        self.db
            .read_with(|snapshot| {
                let c = snapshot.connection();
                let repo = Repository::new(c, self.db.server_id(), self.db.clock().millis());
                if let identity::Decision::Deny(reason) = self
                    .policy(&repo, subject)?
                    .check(Capability::ConfigView, None)
                {
                    return Err(OperationError::forbidden(reason));
                }
                effective(c, self.db.server_id()).map_err(Into::into)
            })
            .map_err(Into::into)
    }
    pub fn set_motd_policy(
        &self,
        subject: &Subject,
        expected: Option<i64>,
        document: Document,
    ) -> OperationsResult<i64> {
        if !document.valid() || expected.is_some_and(|r| r <= 0) {
            return Err(invalid());
        }
        let json = serde_json::to_string(&document).map_err(|_| invalid())?;
        if json.len() > MAX_POLICY_BYTES {
            return Err(invalid());
        }
        let result=self.db.write_with(|transaction|{
            let c=transaction.connection();let repo=Repository::new(c,self.db.server_id(),self.db.clock().millis());
            if let identity::Decision::Deny(reason)=self.policy(&repo,subject)?.check(Capability::ConfigEdit,None){return Err(OperationError::forbidden(reason));}
            let current:Option<i64>=c.query_row("SELECT desired_revision FROM settings WHERE server_id=? AND key=?",rusqlite::params![self.db.server_id(),KEY],|r|r.get(0)).optional()?;
            if current!=expected{return Err(OperationError::rejected(OperationErrorCode::SourceRevisionUnavailable));}
            let revision=current.unwrap_or(0).checked_add(1).ok_or_else(invalid)?;
            c.execute("INSERT INTO settings(server_id,key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at) VALUES(?,?,?,?,'immediate',?,?,?) ON CONFLICT(server_id,key) DO UPDATE SET desired_value=excluded.desired_value,desired_revision=excluded.desired_revision,effective_value=excluded.effective_value,effective_revision=excluded.effective_revision,updated_at=excluded.updated_at",rusqlite::params![self.db.server_id(),KEY,json,revision,json,revision,self.db.clock().millis()])?;
            self.audit.append(c,&AuditRecord::new(request_actor(subject)?,"motd.policy.update".into(),"motd-policy".into(),"accepted".into(),None,[("revision".into(),Some(revision.to_string()))])?)?;
            Ok(revision)
        });
        self.finish_request(result, subject, "motd.policy.update", None)
    }
}
