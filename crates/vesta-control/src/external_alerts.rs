//! Opt-in delivery of the existing notification inbox. No second event engine.
//! Destinations are OS-local secrets; remote users cannot supply arbitrary URLs.
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, future::Future, pin::Pin, sync::Arc};
use vesta_state::{StateDatabase, StateError, artifacts::ArtifactName};

#[derive(Clone, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    pub sinks: Vec<Sink>,
}
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Sink {
    pub id: String,
    url: String,
    #[serde(default)]
    format: Format,
    #[serde(default)]
    include_message: bool,
}
#[derive(Clone, Copy, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Format {
    #[default]
    Json,
    Discord,
}
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalAlerts")
            .field("sink_count", &self.sinks.len())
            .finish()
    }
}
impl Settings {
    pub fn valid(&self) -> bool {
        let mut ids = std::collections::BTreeSet::new();
        self.sinks.len() <= 16
            && self.sinks.iter().all(|s| {
                !s.id.is_empty()
                    && s.id.len() <= 64
                    && s.id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    && ids.insert(&s.id)
                    && url::Url::parse(&s.url).is_ok_and(|u| {
                        u.scheme() == "https"
                            && u.host_str().is_some()
                            && u.username().is_empty()
                            && u.password().is_none()
                            && u.fragment().is_none()
                            && u.port_or_known_default() == Some(443)
                            && u.as_str().len() <= 8192
                    })
            })
    }
}
type SendFuture<'a> =
    Pin<Box<dyn Future<Output = Result<vesta_server_install::outbound::Response, ()>> + Send + 'a>>;
trait Sender: Send + Sync {
    fn post<'a>(&'a self, url: &'a str, body: &'a [u8]) -> SendFuture<'a>;
}
struct Http;
impl Sender for Http {
    fn post<'a>(&'a self, url: &'a str, body: &'a [u8]) -> SendFuture<'a> {
        Box::pin(async move {
            let limits = vesta_server_install::Limits {
                metadata_bytes: 65536,
                connect_ms: 2000,
                idle_ms: 5000,
                total_ms: 5000,
                ..Default::default()
            };
            vesta_server_install::outbound::post_public_json(url, body, limits)
                .await
                .map_err(|_| ())
        })
    }
}
fn invalid() -> StateError {
    StateError::new("state_external_alerts_invalid")
}
struct Deliveries {
    db: StateDatabase,
    settings: Settings,
    sender: Arc<dyn Sender>,
    _lock: File,
    _lease: vesta_state::RuntimeLease,
    stop: Arc<std::sync::atomic::AtomicBool>,
}
impl Deliveries {
    fn open(
        db: StateDatabase,
        settings: Settings,
        sender: Arc<dyn Sender>,
    ) -> vesta_state::Result<Self> {
        if !settings.valid() {
            return Err(invalid());
        }
        let lock = db
            .directory()
            .artifacts()?
            .try_lock_exclusive(&ArtifactName::new("external-alerts.lock")?)
            .map_err(|_| StateError::new("state_external_alerts_busy"))?;
        let lease = db.retain_runtime()?;
        db.write(|c|{
            c.execute_batch("CREATE TABLE IF NOT EXISTS external_alert_delivery(id TEXT PRIMARY KEY,revision TEXT NOT NULL,cursor INTEGER NOT NULL CHECK(cursor>=0),pending INTEGER,state TEXT NOT NULL,attempts INTEGER NOT NULL,next_at INTEGER NOT NULL,delivered INTEGER NOT NULL,failed INTEGER NOT NULL,retention_gap INTEGER NOT NULL) STRICT;")?;
            let current:i64=c.query_row("SELECT COALESCE(MAX(cursor),0) FROM notifications",[],|r|r.get(0))?;
            let keep:std::collections::BTreeSet<_>=settings.sinks.iter().map(|s|s.id.as_str()).collect();
            let old:Vec<String>=c.prepare("SELECT id FROM external_alert_delivery")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for id in old {if !keep.contains(id.as_str()){c.execute("DELETE FROM external_alert_delivery WHERE id=?",[id])?;}}
            for s in &settings.sinks {
                let revision=hex::encode(Sha256::digest(serde_json::to_vec(&(&s.url,s.format,s.include_message)).map_err(|_|invalid())?));
                c.execute("INSERT INTO external_alert_delivery VALUES(?,?,?,NULL,'waiting',0,0,0,0,0) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,cursor=CASE WHEN revision<>excluded.revision OR state='disabled' THEN excluded.cursor ELSE cursor END,pending=CASE WHEN revision<>excluded.revision OR state='disabled' THEN NULL ELSE pending END,state=CASE WHEN revision<>excluded.revision OR state='disabled' THEN 'waiting' WHEN state='sending' THEN 'retry' ELSE state END,attempts=CASE WHEN revision<>excluded.revision OR state='disabled' THEN 0 ELSE attempts END,next_at=CASE WHEN revision<>excluded.revision OR state='disabled' THEN 0 ELSE next_at END",params![s.id,revision,current])?;
            }
            Ok(())
        })?;
        Ok(Self {
            db,
            settings,
            sender,
            _lock: lock,
            _lease: lease,
            stop: Arc::default(),
        })
    }
    async fn tick(&self) -> vesta_state::Result<()> {
        for sink in &self.settings.sinks {
            if self.stop.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let now = self.db.clock().millis();
            let pending=self.db.write(|c|{
                let (cursor,next_at,attempts,pending):(i64,i64,i64,Option<i64>)=c.query_row("SELECT cursor,next_at,attempts,pending FROM external_alert_delivery WHERE id=?",[&sink.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                if now<next_at{return Ok(None);}
                let first:Option<i64>=c.query_row("SELECT MIN(cursor) FROM notifications",[],|r|r.get(0))?;
                if first.is_some_and(|v|cursor>0 && v>cursor.saturating_add(1)) {c.execute("UPDATE external_alert_delivery SET retention_gap=1 WHERE id=?",[&sink.id])?;}
                let notice:Option<(i64,String,i64,String,String,String,String)>=c.query_row("SELECT cursor,id,created_at,severity,title,body,source FROM notifications WHERE cursor>? ORDER BY cursor LIMIT 1",[cursor],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
                let Some(n)=notice else{return Ok(None)};
                let attempt=if pending==Some(n.0){attempts+1}else{1};
                if attempt>3 {
                    c.execute("UPDATE external_alert_delivery SET cursor=?,pending=NULL,state='failed',attempts=0,failed=failed+1 WHERE id=?",params![n.0,sink.id])?;
                    return Ok(None);
                }
                // Intent before I/O. Reopen retries the same event identity;
                // remote acceptance followed by a lost reply can still duplicate.
                c.execute("UPDATE external_alert_delivery SET pending=?,state='sending',attempts=? WHERE id=?",params![n.0,attempt,sink.id])?;
                Ok(Some(n))
            })?;
            let Some(n) = pending else { continue };
            let title = if sink.include_message {
                n.4.as_str()
            } else {
                "Vesta operational notification"
            };
            let body = if sink.include_message {
                n.5.as_str()
            } else {
                "Open Vesta to view details."
            };
            let payload = match sink.format {
                Format::Json => {
                    serde_json::json!({"schema":1,"event_id":n.1,"created_at_ms":n.2,"severity":n.3,"title":title,"body":body})
                }
                // Mentions are disabled even for user-authored workflow text.
                Format::Discord => {
                    serde_json::json!({"content":format!("[{severity}] {title}\n{body}\nEvent: {id}",severity=n.3,id=n.1),"allowed_mentions":{"parse":[]}})
                }
            };
            let mut bytes = serde_json::to_vec(&payload).map_err(|_| invalid())?;
            // Discord's content limit is separate from the inbox contract. Do
            // not truncate Unicode or silently claim complete message delivery.
            if matches!(sink.format, Format::Discord)
                && payload["content"]
                    .as_str()
                    .is_some_and(|s| s.encode_utf16().count() > 2000)
            {
                bytes=serde_json::to_vec(&serde_json::json!({"content":format!("Vesta notification ({severity}). Open Vesta for the full message. Event: {id}",severity=n.3,id=n.1),"allowed_mentions":{"parse":[]}})).map_err(|_|invalid())?;
            }
            let result = self.sender.post(&sink.url, &bytes).await;
            let delivered = result.is_ok_and(|s| (200..300).contains(&s.status));
            let retry = result.is_err()
                || result.is_ok_and(|s| {
                    (s.status == 429 || s.status >= 500) && !s.retry_after_unsupported
                });
            let retry_delay_ms = result
                .ok()
                .and_then(|s| s.retry_after_seconds)
                .unwrap_or(30)
                .max(30)
                .saturating_mul(1000) as i64;
            self.db.write(|c|{
                if delivered || !retry {
                    c.execute("UPDATE external_alert_delivery SET cursor=?,pending=NULL,state=?,attempts=0,delivered=delivered+?,failed=failed+? WHERE id=? AND pending=?",params![n.0,if delivered{"sent"}else{"failed"},i64::from(delivered),i64::from(!delivered),sink.id,n.0])?;
                }else{
                    c.execute("UPDATE external_alert_delivery SET state='retry',next_at=? WHERE id=? AND pending=?",params![self.db.clock().millis().saturating_add(retry_delay_ms),sink.id,n.0])?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

pub(crate) struct Worker {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    pub(crate) fn start(
        db: StateDatabase,
        settings: Settings,
    ) -> vesta_state::Result<Option<Self>> {
        if settings.sinks.is_empty() {
            db.write(|c|{
                let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='external_alert_delivery')",[],|r|r.get(0))?;
                if exists {c.execute("UPDATE external_alert_delivery SET state='disabled',pending=NULL,attempts=0,next_at=0,cursor=(SELECT COALESCE(MAX(cursor),0) FROM notifications)",[])?;}
                Ok(())
            })?;
            return Ok(None);
        }
        let delivery = Deliveries::open(db, settings, Arc::new(Http))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| StateError::new("state_external_alerts_unavailable"))?;
        let stop = delivery.stop.clone();
        let signal = stop.clone();
        let thread = std::thread::Builder::new()
            .name("vesta-external-alerts".into())
            .spawn(move || {
                runtime.block_on(async move {
                    while !signal.load(std::sync::atomic::Ordering::Acquire) {
                        let _ = delivery.tick().await;
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                })
            })
            .map_err(|_| StateError::new("state_external_alerts_unavailable"))?;
        Ok(Some(Self {
            stop,
            thread: Some(thread),
        }))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests;

#[derive(Serialize)]
pub struct DeliveryStatus {
    pub sink_id: String,
    pub state: String,
    pub cursor: i64,
    pub pending_cursor: Option<i64>,
    pub attempts: i64,
    pub next_attempt_at_ms: i64,
    pub delivered: i64,
    pub failed: i64,
    pub retention_gap: bool,
}
pub(crate) fn status_in(c: &rusqlite::Connection) -> vesta_state::Result<Vec<DeliveryStatus>> {
    let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='external_alert_delivery')",[],|r|r.get(0))?;
    if !exists {
        return Ok(vec![]);
    }
    Ok(c.prepare("SELECT id,state,cursor,pending,attempts,next_at,delivered,failed,retention_gap FROM external_alert_delivery ORDER BY id LIMIT 16")?
        .query_map([],|r|Ok(DeliveryStatus{sink_id:r.get(0)?,state:r.get(1)?,cursor:r.get(2)?,pending_cursor:r.get(3)?,attempts:r.get(4)?,next_attempt_at_ms:r.get(5)?,delivered:r.get(6)?,failed:r.get(7)?,retention_gap:r.get(8)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
}
