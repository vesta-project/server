//! Optional SSH/SFTP service. Opening this facade never spawns a listener.
mod config;
mod protocol;
mod ssh;
#[cfg(test)]
mod tests;
pub use config::{Config, ConfigPatch, Limits, LimitsPatch};

use crate::{
    files::FileBrowser,
    identity::management::ManagementAccess,
    identity::{self, BuiltinRole, Capability, DeviceId, PrincipalId, Subject},
};
use rusqlite::{Connection, OptionalExtension, params};
use russh::keys::ssh_key::{self, PrivateKey, PublicKey};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{
    sync::{Semaphore, watch},
    task::JoinHandle,
};
use vesta_state::{
    StateDatabase, StateError, TransactionError,
    audit::{AuditActor, AuditLog, AuditRecord},
};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error(&'static str);
impl Error {
    pub fn code(&self) -> &'static str {
        self.0
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
impl From<StateError> for Error {
    fn from(_: StateError) -> Self {
        Self("sftp_state_unavailable")
    }
}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self("sftp_state_unavailable")
    }
}
impl From<crate::ControlError> for Error {
    fn from(_: crate::ControlError) -> Self {
        Self("sftp_forbidden")
    }
}
impl From<TransactionError<Error>> for Error {
    fn from(e: TransactionError<Error>) -> Self {
        match e {
            TransactionError::Work(e) => e,
            _ => Self("sftp_state_unavailable"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRegistration {
    pub principal_id: String,
    pub label: String,
    pub public_key: String,
    pub expires_at_ms: Option<i64>,
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    address: SocketAddr,
}
#[derive(Default)]
struct Budgets {
    sessions: HashMap<std::net::IpAddr, usize>,
    transfers: HashMap<String, usize>,
    attempts: HashMap<std::net::IpAddr, (Instant, u32)>,
    traffic: HashMap<String, (Instant, u64)>,
}

pub struct SftpService {
    owner_runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    db: StateDatabase,
    files: FileBrowser,
    management: ManagementAccess,
    audit: AuditLog,
    running: Mutex<Option<Running>>,
    lifecycle: tokio::sync::Mutex<()>,
    budgets: Mutex<Budgets>,
    io: Arc<Semaphore>,
}
impl Drop for SftpService {
    fn drop(&mut self) {
        if let Ok(running) = self.running.get_mut() {
            if let Some(running) = running.take() {
                let _ = running.stop.send(true);
                running.task.abort();
            }
        }
    }
}

impl SftpService {
    /// Reserve before any effect. An interrupted reservation is never replayed.
    /// Retained receipts are bounded and never evicted to admit duplicate work.
    pub fn begin_mutation(
        &self,
        token: &str,
        cap: Capability,
        key: &str,
        route: &str,
        input: &[u8],
    ) -> Result<Option<Result<Value>>> {
        if key.is_empty() || key.len() > 512 || input.len() > 65536 {
            return Err(Error("sftp_request_invalid"));
        }
        let subject = self.authenticated_owner(token, cap)?;
        let mut hash = Sha256::new();
        hash.update(route.as_bytes());
        hash.update([0]);
        hash.update(input);
        let hash = hex::encode(hash.finalize());
        self.db.write_with(|tx| {
            let c=tx.connection();self.owner_in(c,&subject,cap)?;
            let existing:Option<(String,String,String,Option<String>)>=c.query_row("SELECT principal_id,device_id,input_hash,result_json FROM sftp_management_outcomes WHERE server_id=? AND request_key=?",params![self.db.server_id(),key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if let Some((principal,device,old_hash,result))=existing {
                if principal!=subject.principal.as_str()||device!=subject.device.as_str()||old_hash!=hash {return Err(Error("sftp_idempotency_conflict"));}
                let result=result.ok_or(Error("sftp_outcome_unknown"))?;
                let value:Value=serde_json::from_str(&result).map_err(|_|Error("sftp_state_unavailable"))?;
                return Ok(Some(if let Some(data)=value.get("data"){Ok(data.clone())}else{Err(Error("sftp_previous_failure"))}));
            }
            let count:i64=c.query_row("SELECT count(*) FROM sftp_management_outcomes WHERE server_id=?",[self.db.server_id()],|r|r.get(0))?;
            if count>=4096 {return Err(Error("sftp_receipts_full"));}
            c.execute("INSERT INTO sftp_management_outcomes(server_id,request_key,principal_id,device_id,input_hash) VALUES(?,?,?,?,?)",params![self.db.server_id(),key,subject.principal.as_str(),subject.device.as_str(),hash])?;
            Ok(None)
        }).map_err(Error::from)
    }
    pub fn finish_mutation(&self, key: &str, result: &Result<Value>) -> Result<()> {
        let result = match result {
            Ok(value) => json!({"data":value}),
            Err(error) => json!({"error":error.code()}),
        };
        let text = serde_json::to_string(&result).map_err(|_| Error("sftp_state_unavailable"))?;
        if text.len() > 65536 {
            return Err(Error("sftp_outcome_unknown"));
        }
        self.db.write(|c| {c.execute("UPDATE sftp_management_outcomes SET result_json=? WHERE server_id=? AND request_key=? AND result_json IS NULL",params![text,self.db.server_id(),key])?;Ok(())})?;
        Ok(())
    }
    pub fn set_owner_runtime(&self, runtime: tokio::runtime::Handle) {
        let _ = self.owner_runtime.set(runtime);
    }
    pub fn owner_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.owner_runtime.get().cloned()
    }

    /// Service-owned schema initialization commits atomically. All users/roles
    /// remain canonical; each SSH credential is a separately revocable device.
    pub fn open(
        db: StateDatabase,
        files: FileBrowser,
        management: ManagementAccess,
    ) -> Result<Arc<Self>> {
        if management.server_id() != db.server_id() {
            return Err(Error("sftp_state_unavailable"));
        }
        db.write(|c| {
            c.execute_batch(include_str!("schema.sql"))?;
            Ok(())
        })?;
        Ok(Arc::new(Self {
            owner_runtime: std::sync::OnceLock::new(),
            management,
            audit: AuditLog::new(db.clone())?,
            db,
            files: files.for_remote(),
            running: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
            budgets: Mutex::new(Budgets::default()),
            io: Arc::new(Semaphore::new(4)),
        }))
    }
    fn config_in(&self, c: &Connection) -> Result<Config> {
        let text: Option<String> = c
            .query_row(
                "SELECT config_json FROM sftp_configuration WHERE server_id=?",
                [self.db.server_id()],
                |r| r.get(0),
            )
            .optional()?;
        let config = text
            .map(|s| serde_json::from_str(&s).map_err(|_| Error("sftp_configuration_invalid")))
            .transpose()?
            .unwrap_or_default();
        Config::validate(&config)?;
        Ok(config)
    }
    pub fn configuration(&self) -> Result<Config> {
        self.db
            .read_with(|tx| self.config_in(tx.connection()))
            .map_err(Error::from)
    }
    fn owner_in(&self, c: &Connection, subject: &Subject, cap: Capability) -> Result<()> {
        let policy = identity::policy_in(
            c,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(subject),
        )?;
        if !policy.holds_builtin(BuiltinRole::Owner) || !policy.check(cap, None).allowed() {
            return Err(Error("sftp_forbidden"));
        }
        Ok(())
    }
    fn actor(subject: &Subject) -> Result<AuditActor> {
        Ok(AuditActor::new(
            subject.principal.as_str().into(),
            subject.device.as_str().into(),
            "sftp".into(),
        )?)
    }
    fn record(&self, c: &Connection, subject: &Subject, action: &str, outcome: &str) -> Result<()> {
        self.audit.append(
            c,
            &AuditRecord::new(
                Self::actor(subject)?,
                action.into(),
                "sftp".into(),
                outcome.into(),
                None,
                [],
            )?,
        )?;
        Ok(())
    }
    fn authenticated_owner(&self, token: &str, cap: Capability) -> Result<Subject> {
        self.management
            .authorize(token, cap, None)
            .map_err(|_| Error("sftp_forbidden"))
    }
    pub fn status(&self, token: &str) -> Result<Value> {
        let subject = self.authenticated_owner(token, Capability::ConfigView)?;
        self.db
            .read_with(|tx| {
                self.owner_in(tx.connection(), &subject, Capability::ConfigView)?;
                let mut status = self.status_in(tx.connection())?;
                status["username"] = json!(subject.principal.as_str());
                Ok(status)
            })
            .map_err(Error::from)
    }
    fn status_in(&self, c: &Connection) -> Result<Value> {
        let running = self.running.lock().map_err(|_| Error("sftp_unavailable"))?;
        let sessions: usize = self
            .budgets
            .lock()
            .map_err(|_| Error("sftp_unavailable"))?
            .sessions
            .values()
            .sum();
        let fingerprint: Option<String> = c
            .query_row(
                "SELECT fingerprint FROM sftp_host_key WHERE server_id=?",
                [self.db.server_id()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(
            json!({"config":self.config_in(c)?,"running":running.as_ref().is_some_and(|r|!r.task.is_finished()),"bound_address":running.as_ref().map(|r|r.address.to_string()),"host_key_fingerprint":fingerprint,"active_sessions":sessions,"authentication":["publickey"],"username":"canonical_principal_uuid"}),
        )
    }
    /// Persist + apply atomically with respect to other configuration requests.
    /// Bind failure is returned, with desired enabled retained and running false.
    pub async fn patch(self: &Arc<Self>, token: &str, patch: ConfigPatch) -> Result<Value> {
        let subject = self.authenticated_owner(token, Capability::SecurityManage)?;
        let _guard = self.lifecycle.lock().await;
        self.db.write_with(|tx| {
            let c=tx.connection(); self.owner_in(c,&subject,Capability::SecurityManage)?;
            let mut config=self.config_in(c)?; patch.apply(&mut config); config.validate()?;
            c.execute("INSERT INTO sftp_configuration(server_id,config_json) VALUES (?,?) ON CONFLICT(server_id) DO UPDATE SET config_json=excluded.config_json",params![self.db.server_id(),serde_json::to_string(&config).map_err(|_|Error("sftp_configuration_invalid"))?])?;
            self.record(c,&subject,"sftp.configure","success")
        }).map_err(Error::from)?;
        self.stop_locked().await;
        self.start_locked().await?;
        self.status(token)
    }
    pub fn keys(&self, token: &str) -> Result<Value> {
        let subject = self.authenticated_owner(token, Capability::CredentialsManage)?;
        self.db.read_with(|tx| {
            let c=tx.connection(); self.owner_in(c,&subject,Capability::CredentialsManage)?;
            let mut q=c.prepare("SELECT id FROM sftp_keys WHERE server_id=? ORDER BY created_at,id LIMIT 256")?;
            let ids=q.query_map([self.db.server_id()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"keys":ids.iter().map(|id|self.key_in(c,id)).collect::<Result<Vec<_>>>()?}))
        }).map_err(Error::from)
    }
    fn key_in(&self, c: &Connection, id: &str) -> Result<Value> {
        Ok(c.query_row("SELECT id,principal_id,label,public_key,fingerprint,created_at,expires_at,revoked_at FROM sftp_keys WHERE server_id=? AND id=?",params![self.db.server_id(),id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"principal_id":r.get::<_,String>(1)?,"label":r.get::<_,String>(2)?,"public_key":r.get::<_,String>(3)?,"fingerprint":r.get::<_,String>(4)?,"created_at_ms":r.get::<_,i64>(5)?,"expires_at_ms":r.get::<_,Option<i64>>(6)?,"revoked_at_ms":r.get::<_,Option<i64>>(7)?})))?)
    }
    pub fn register_key(&self, token: &str, input: KeyRegistration) -> Result<Value> {
        let subject = self.authenticated_owner(token, Capability::CredentialsManage)?;
        let principal = PrincipalId::parse(&input.principal_id)?;
        if input.public_key.len() > 4096
            || input.label.is_empty()
            || input.label.len() > 96
            || input.label.chars().any(char::is_control)
        {
            return Err(Error("sftp_key_invalid"));
        }
        let key =
            PublicKey::from_openssh(&input.public_key).map_err(|_| Error("sftp_key_invalid"))?;
        if key.algorithm() != ssh_key::Algorithm::Ed25519 {
            return Err(Error("sftp_key_algorithm_unsupported"));
        }
        let wire = key.to_bytes().map_err(|_| Error("sftp_key_invalid"))?;
        let canonical = PublicKey::from_bytes(&wire)
            .map_err(|_| Error("sftp_key_invalid"))?
            .to_openssh()
            .map_err(|_| Error("sftp_key_invalid"))?;
        let fingerprint = key.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        let hash = hex::encode(Sha256::digest(&wire));
        self.db.write_with(|tx| {
            let c=tx.connection(); self.owner_in(c,&subject,Capability::CredentialsManage)?;
            let now=self.db.clock().millis();
            if input.expires_at_ms.is_some_and(|e|e<=now) { return Err(Error("sftp_key_expiry_invalid")); }
            let count:i64=c.query_row("SELECT COUNT(*) FROM sftp_keys WHERE server_id=?",[self.db.server_id()],|r|r.get(0))?;
            if count>=256 { return Err(Error("sftp_keys_full")); }
            let active:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM principals WHERE server_id=? AND id=? AND status='active')",params![self.db.server_id(),principal.as_str()],|r|r.get(0))?;
            if !active { return Err(Error("sftp_principal_invalid")); }
            // Cross-protocol public material is domain separated in canonical
            // devices; a separate method cannot sign management sessions.
            let id=uuid::Uuid::new_v4().to_string();
            c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES(?,?,?,?,?,?)",params![id,self.db.server_id(),principal.as_str(),input.label,hash,now]).map_err(|_|Error("sftp_key_duplicate"))?;
            c.execute("INSERT INTO sftp_keys(id,server_id,principal_id,label,public_key,fingerprint,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?)",params![id,self.db.server_id(),principal.as_str(),input.label,canonical,fingerprint,now,input.expires_at_ms])?;
            self.record(c,&subject,"sftp.key.register","success")?; self.key_in(c,&id)
        }).map_err(Error::from)
    }
    pub fn revoke_key(&self, token: &str, id: &str) -> Result<Value> {
        let subject = self.authenticated_owner(token, Capability::CredentialsManage)?;
        let id = DeviceId::parse(id)?;
        self.db.write_with(|tx| {
            let c=tx.connection(); self.owner_in(c,&subject,Capability::CredentialsManage)?;
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sftp_keys WHERE server_id=? AND id=?)",params![self.db.server_id(),id.as_str()],|r|r.get(0))?;
            if !exists {return Err(Error("sftp_key_not_found"));}
            c.execute("UPDATE sftp_keys SET revoked_at=COALESCE(revoked_at,?) WHERE server_id=? AND id=?",params![self.db.clock().millis(),self.db.server_id(),id.as_str()])?;
            c.execute("UPDATE devices SET revoked_at=COALESCE(revoked_at,?),revoked_by=? WHERE server_id=? AND id=?",params![self.db.clock().millis(),subject.principal.as_str(),self.db.server_id(),id.as_str()])?;
            self.record(c,&subject,"sftp.key.revoke","success")?; Ok(json!({"revoked":true}))
        }).map_err(Error::from)
    }
    fn credential_in(&self, c: &Connection, username: &str, fingerprint: &str) -> Result<Subject> {
        let principal = PrincipalId::parse(username)?;
        let id:Option<String>=c.query_row("SELECT k.id FROM sftp_keys k JOIN devices d ON d.id=k.id WHERE k.server_id=? AND k.principal_id=? AND k.fingerprint=? AND k.revoked_at IS NULL AND d.revoked_at IS NULL AND (k.expires_at IS NULL OR k.expires_at>?)",params![self.db.server_id(),principal.as_str(),fingerprint,self.db.clock().millis()],|r|r.get(0)).optional()?;
        let subject = Subject {
            principal,
            device: DeviceId::parse(&id.ok_or(Error("sftp_authentication_failed"))?)?,
        };
        let p = identity::policy_in(
            c,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(&subject),
        )?;
        if !p.has_current_identity() {
            return Err(Error("sftp_authentication_failed"));
        }
        Ok(subject)
    }
    fn credential(&self, username: &str, fingerprint: &str) -> Result<Subject> {
        self.db
            .read_with(|tx| self.credential_in(tx.connection(), username, fingerprint))
            .map_err(Error::from)
    }
    fn recheck(&self, subject: &Subject) -> Result<()> {
        self.db
            .read_with(|tx| {
                let c = tx.connection();
                let fingerprint: String = c.query_row(
                    "SELECT fingerprint FROM sftp_keys WHERE server_id=? AND id=?",
                    params![self.db.server_id(), subject.device.as_str()],
                    |r| r.get(0),
                )?;
                if self.credential_in(c, subject.principal.as_str(), &fingerprint)? != *subject {
                    return Err(Error("sftp_authentication_failed"));
                }
                Ok(())
            })
            .map_err(Error::from)
    }
    fn reserve_traffic(&self, subject: &Subject, bytes: u64, limit: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        self.db.write_with(|tx| {
            let c=tx.connection();
            let day=self.db.clock().millis().div_euclid(86_400_000);
            let previous:Option<(i64,i64)>=c.query_row("SELECT day,bytes FROM sftp_transfer_usage WHERE server_id=? AND principal_id=?",params![self.db.server_id(),subject.principal.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            // Clock rollback never resets a consumed quota. Only a later UTC day
            // resets it. One retained record per registered canonical principal.
            let (day,used)=previous.map(|(d,b)|(day.max(d),if day>d {0}else{b as u64})).unwrap_or((day,0));
            let used=used.checked_add(bytes).filter(|b|*b<=limit).ok_or(Error("sftp_daily_quota_exceeded"))?;
            c.execute("INSERT INTO sftp_transfer_usage(server_id,principal_id,day,bytes) VALUES(?,?,?,?) ON CONFLICT(server_id,principal_id) DO UPDATE SET day=excluded.day,bytes=excluded.bytes",params![self.db.server_id(),subject.principal.as_str(),day,used as i64])?; Ok(())
        }).map_err(Error::from)
    }
    fn rate_delay(&self, subject: &Subject, bytes: u64, limit: u64) -> Result<std::time::Duration> {
        if bytes == 0 {
            return Ok(std::time::Duration::ZERO);
        }
        let mut budgets = self.budgets.lock().map_err(|_| Error("sftp_unavailable"))?;
        let now = Instant::now();
        budgets.traffic.retain(|_, (t, _)| {
            now.saturating_duration_since(*t) < std::time::Duration::from_secs(2)
        });
        let (next, _) = budgets
            .traffic
            .entry(subject.principal.as_str().into())
            .or_insert((now, 0));
        let start = (*next).max(now);
        *next = start + std::time::Duration::from_secs_f64(bytes as f64 / limit as f64);
        Ok(start.saturating_duration_since(now))
    }
    fn host_key(&self) -> Result<PrivateKey> {
        self.db
            .write_with(|tx| {
                let c = tx.connection();
                let existing: Option<String> = c
                    .query_row(
                        "SELECT private_key FROM sftp_host_key WHERE server_id=?",
                        [self.db.server_id()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = existing {
                    return PrivateKey::from_openssh(existing)
                        .map_err(|_| Error("sftp_host_key_invalid"));
                }
                let key = PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
                    .map_err(|_| Error("sftp_host_key_unavailable"))?;
                let secret = key
                    .to_openssh(ssh_key::LineEnding::LF)
                    .map_err(|_| Error("sftp_host_key_unavailable"))?;
                c.execute(
                    "INSERT INTO sftp_host_key(server_id,private_key,fingerprint) VALUES(?,?,?)",
                    params![
                        self.db.server_id(),
                        secret.as_str(),
                        key.public_key()
                            .fingerprint(ssh_key::HashAlg::Sha256)
                            .to_string()
                    ],
                )?;
                Ok(key)
            })
            .map_err(Error::from)
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.set_owner_runtime(tokio::runtime::Handle::current());
        let _guard = self.lifecycle.lock().await;
        self.start_locked().await
    }
    async fn start_locked(self: &Arc<Self>) -> Result<()> {
        if self
            .running
            .lock()
            .map_err(|_| Error("sftp_unavailable"))?
            .is_some()
        {
            return Ok(());
        }
        let config = self.configuration()?;
        if !config.enabled {
            return Ok(());
        }
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(config.bind_address, config.port))
                .await
                .map_err(|_| Error("sftp_bind_failed"))?;
        let address = listener
            .local_addr()
            .map_err(|_| Error("sftp_bind_failed"))?;
        let key = self.host_key()?;
        let (stop, rx) = watch::channel(false);
        let service = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            ssh::listen(service, listener, config, key, rx).await;
        });
        *self.running.lock().map_err(|_| Error("sftp_unavailable"))? = Some(Running {
            stop,
            task,
            address,
        });
        Ok(())
    }
    pub async fn shutdown(&self) {
        let _guard = self.lifecycle.lock().await;
        self.stop_locked().await;
    }
    async fn stop_locked(&self) {
        let running = self.running.lock().ok().and_then(|mut r| r.take());
        if let Some(mut running) = running {
            let _ = running.stop.send(true);
            if tokio::time::timeout(std::time::Duration::from_secs(5), &mut running.task)
                .await
                .is_err()
            {
                running.task.abort();
                let _ = running.task.await;
            }
        }
    }
    async fn blocking<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self
            .io
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error("sftp_io_busy"))?;
        let service = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work(service)
        })
        .await
        .map_err(|_| Error("sftp_io_failed"))?
    }
}
