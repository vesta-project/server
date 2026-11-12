//! Installation-local reservation of exactly one account-bound first server.
//! This is the authority foundation for pending-host onboarding, not an installer
//! or a claim that the launcher workflow is connected. No update/isolation or
//! machine-wide operation is exposed here. Existing enrollment/session proof is
//! reused; no setup-code-only or accountless remote authentication is introduced.
use crate::identity::{
    Subject,
    management::{ManagementAccess, permissions::PermissionError},
};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use vesta_state::{
    StateDatabase, TransactionError,
    audit::{AuditActor, AuditLog, AuditRecord},
};

pub mod consumer;
pub mod provisioning;

/// Compiled IPC identity for signed updater capabilities. No host observation,
/// database opening, or native/runtime effects.
pub fn compiled_supervisor_ipc() -> u8 {
    vesta_supervisor::wire::VERSION
}

type Result<T> = std::result::Result<T, PermissionError>;

#[derive(Clone)]
pub struct FirstServerSetup {
    db: StateDatabase,
    management: ManagementAccess,
    installation_id: String,
    audit: AuditLog,
}
impl std::fmt::Debug for FirstServerSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FirstServerSetup")
    }
}

/// No paths, invitation codes, account identifiers or bearer credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FirstServerStatus {
    pub installation_id: String,
    pub server_id: String,
    pub claimed: bool,
}

impl FirstServerSetup {
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }
    /// Trusted OS-local composition only, before exposing a pending listener.
    /// Reopening the same reservation is safe; reusing an established server or
    /// changing installation identity refuses without modifying its principals.
    pub fn reserve_local(db: StateDatabase, installation_id: &str) -> Result<Self> {
        if installation_id.len() != 32
            || !installation_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PermissionError::InvalidRequest);
        }
        match db.write_with(|tx| -> Result<()> {
            let c=tx.connection();
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='first_server_setup')",[],|r|r.get(0))?;
            if !exists {
                let users:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM principals)",[],|r|r.get(0))?;
                if users { return Err(PermissionError::Conflict); }
                c.execute_batch("CREATE TABLE first_server_setup(singleton INTEGER PRIMARY KEY CHECK(singleton=1),installation_id TEXT NOT NULL,server_id TEXT NOT NULL,owner_principal TEXT,owner_account TEXT,claimed_at_ms INTEGER,CHECK((owner_principal IS NULL AND owner_account IS NULL AND claimed_at_ms IS NULL) OR (owner_principal IS NOT NULL AND owner_account IS NOT NULL AND claimed_at_ms IS NOT NULL)))")?;
                c.execute("INSERT INTO first_server_setup(singleton,installation_id,server_id) VALUES(1,?,?)",params![installation_id,db.server_id()])?;
            }
            let (installation,server):(String,String)=c.query_row("SELECT installation_id,server_id FROM first_server_setup WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if installation!=installation_id || server!=db.server_id() {return Err(PermissionError::Conflict);}
            Ok(())
        }) {
            Ok(())=>(), Err(TransactionError::Work(e))=>return Err(e), Err(_)=>return Err(PermissionError::Unavailable),
        }
        let management = ManagementAccess::new(db.clone())?;
        let audit = AuditLog::new(db.clone()).map_err(|_| PermissionError::Unavailable)?;
        Ok(Self {
            db,
            management,
            installation_id: installation_id.into(),
            audit,
        })
    }

    /// One long-lived session store is shared with the pending transport's
    /// existing account enrollment/challenge/session handlers.
    pub fn management(&self) -> &ManagementAccess {
        &self.management
    }

    pub fn claimed_status(&self, token: &str) -> Result<FirstServerStatus> {
        self.with_owner(token, |_, _, _| Ok(self.status(true)))
    }

    /// Claim by a current account-linked Owner. A lost reply/repeated request by
    /// that account returns the same result; another account cannot replace it.
    pub fn claim(&self, token: &str) -> Result<FirstServerStatus> {
        self.management.owner_transaction(token, |c,subject,now| {
            let owner=self.binding(c)?;
            let account=self.account(c,subject)?;
            if let Some((principal,uuid))=owner {
                if principal!=subject.principal.as_str() || uuid!=account { return Err(PermissionError::Conflict); }
                return Ok(self.status(true));
            }
            c.execute("UPDATE first_server_setup SET owner_principal=?,owner_account=?,claimed_at_ms=? WHERE singleton=1 AND owner_principal IS NULL",params![subject.principal.as_str(),account,now])?;
            let record=AuditRecord::new(
                AuditActor::new(subject.principal.to_string(),subject.device.to_string(),"setup".into()).map_err(|_|PermissionError::Unavailable)?,
                "setup.first_server.claim".into(),self.db.server_id().into(),"success".into(),None,
                std::iter::empty::<(String,Option<String>)>(),
            ).map_err(|_|PermissionError::Unavailable)?;
            self.audit.append(c,&record).map_err(|_|PermissionError::Unavailable)?;
            Ok(self.status(true))
        })
    }

    /// Setup mutations must call this in their transaction rather than retain a
    /// successful status query as a reusable authorization permit.
    pub(crate) fn with_owner<T>(
        &self,
        token: &str,
        work: impl FnOnce(&rusqlite::Connection, &Subject, i64) -> Result<T>,
    ) -> Result<T> {
        self.management.owner_transaction(token, |c, subject, now| {
            let owner = self
                .binding(c)?
                .ok_or(PermissionError::PreconditionFailed)?;
            if owner != (subject.principal.to_string(), self.account(c, subject)?) {
                return Err(PermissionError::Forbidden);
            }
            work(c, subject, now)
        })
    }

    fn binding(&self, c: &rusqlite::Connection) -> Result<Option<(String, String)>> {
        let row:(String,String,Option<String>,Option<String>)=c.query_row("SELECT installation_id,server_id,owner_principal,owner_account FROM first_server_setup WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if row.0 != self.installation_id || row.1 != self.db.server_id() {
            return Err(PermissionError::Conflict);
        }
        match (row.2, row.3) {
            (None, None) => Ok(None),
            (Some(p), Some(a)) => Ok(Some((p, a))),
            _ => Err(PermissionError::Unavailable),
        }
    }
    fn account(&self, c: &rusqlite::Connection, subject: &Subject) -> Result<String> {
        c.query_row("SELECT a.minecraft_uuid FROM user_minecraft_accounts a JOIN management_device_keys k ON k.server_id=a.server_id AND k.linked_minecraft_uuid=a.minecraft_uuid WHERE a.server_id=? AND a.principal_id=? AND k.device_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str()],|r|r.get(0)).optional()?.ok_or(PermissionError::SessionInvalid)
    }
    fn status(&self, claimed: bool) -> FirstServerStatus {
        FirstServerStatus {
            installation_id: self.installation_id.clone(),
            server_id: self.db.server_id().into(),
            claimed,
        }
    }
}

#[cfg(test)]
mod tests;
