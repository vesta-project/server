use super::{Automation, Severity, model::bounded_text};
use crate::{
    ControlError, IdentityErrorCode, Result,
    identity::{Capability, Subject},
};
use rusqlite::{Connection, params};

pub(crate) const MAX_NOTIFICATIONS: i64 = 4096;
pub(crate) const HISTORY_MS: i64 = 90 * 24 * 60 * 60 * 1000;
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub cursor: i64,
    pub id: String,
    pub created_at: i64,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub source: String,
    pub read: bool,
    pub acknowledged: bool,
}
/// `oldest_cursor` lets offline devices detect a retention gap. Cursors remain
/// monotonic after deletion. `next_cursor` advances only over returned entries.
#[derive(Clone, Debug, PartialEq)]
pub struct NotificationPage {
    pub notifications: Vec<Notification>,
    pub next_cursor: i64,
    pub oldest_cursor: Option<i64>,
}
impl Automation {
    /// Trusted system producer; not a caller-authorized transport command.
    pub fn notify(
        &self,
        severity: Severity,
        title: &str,
        body: &str,
        source: &str,
    ) -> Result<String> {
        self.write(|c| insert(c, self.now(), severity, title, body, source))
    }
    /// Subject binds the principal to the transport's admitted device. All of a
    /// principal's devices share read and acknowledgment state.
    pub fn list_since(
        &self,
        subject: &Subject,
        cursor: i64,
        limit: usize,
    ) -> Result<NotificationPage> {
        super::service::page(limit)?;
        if cursor < 0 {
            return Err(super::service::invalid());
        }
        self.read(|c| {
            self.authorize(c, subject, Capability::NotificationsView)?;
            let oldest_cursor = c.query_row("SELECT min(cursor) FROM notifications", [], |r| r.get(0))?;
            let mut q = c.prepare("SELECT n.cursor,n.id,n.created_at,n.severity,n.title,n.body,n.source,s.read_at IS NOT NULL,s.acknowledged_at IS NOT NULL FROM notifications n LEFT JOIN notification_state s ON s.notification_id=n.id AND s.principal_id=? WHERE n.cursor>? ORDER BY n.cursor LIMIT ?")?;
            let notifications = q.query_map(params![subject.principal.as_str(),cursor,limit as i64], |r| {
                let severity: String = r.get(3)?;
                let severity = match severity.as_str() {
                    "info" => Severity::Info, "warning" => Severity::Warning, "critical" => Severity::Critical,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(Notification {cursor:r.get(0)?, id:r.get(1)?,created_at:r.get(2)?,severity,title:r.get(4)?,body:r.get(5)?,source:r.get(6)?,read:r.get(7)?,acknowledged:r.get(8)?})
            })?.collect::<std::result::Result<Vec<_>,_>>()?;
            let next_cursor = notifications.last().map_or(cursor, |n| n.cursor);
            Ok(NotificationPage {notifications,next_cursor,oldest_cursor})
        })
    }
    pub fn unread_count(&self, subject: &Subject) -> Result<u64> {
        self.read(|c| {
            self.authorize(c,subject,Capability::NotificationsView)?;
            Ok(c.query_row("SELECT count(*) FROM notifications n WHERE NOT EXISTS(SELECT 1 FROM notification_state s WHERE s.notification_id=n.id AND s.principal_id=? AND s.read_at IS NOT NULL)", [subject.principal.as_str()], |r| r.get::<_,i64>(0))? as u64)
        })
    }
    pub fn mark_read(&self, subject: &Subject, ids: &[String]) -> Result<()> {
        self.mark(subject, ids, false)
    }
    /// Acknowledging also marks read, and is idempotent.
    pub fn acknowledge(&self, subject: &Subject, ids: &[String]) -> Result<()> {
        self.mark(subject, ids, true)
    }
    fn mark(&self, subject: &Subject, ids: &[String], acknowledge: bool) -> Result<()> {
        self.write(|c| self.mark_in(c, subject, ids, acknowledge))
    }
    pub(super) fn mark_in(
        &self,
        c: &Connection,
        subject: &Subject,
        ids: &[String],
        acknowledge: bool,
    ) -> Result<()> {
        if ids.is_empty() || ids.len() > 256 || ids.iter().any(|id| !super::service::valid_id(id)) {
            return Err(super::service::invalid());
        }
        self.authorize(c, subject, Capability::NotificationsView)?;
        for id in ids {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM notifications WHERE id=?)",
                [id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(ControlError::rejected(IdentityErrorCode::NotFound));
            }
            c.execute("INSERT INTO notification_state(principal_id,notification_id,read_at,acknowledged_at) VALUES (?,?,?,?) ON CONFLICT(principal_id,notification_id) DO UPDATE SET read_at=coalesce(read_at,excluded.read_at),acknowledged_at=coalesce(acknowledged_at,excluded.acknowledged_at)",params![subject.principal.as_str(),id,self.now(),acknowledge.then(||self.now())])?;
        }
        Ok(())
    }
}
pub(crate) fn insert(
    c: &Connection,
    now: i64,
    severity: Severity,
    title: &str,
    body: &str,
    source: &str,
) -> Result<String> {
    if now < 0
        || !bounded_text(title, 256, false)
        || !bounded_text(body, 8192, true)
        || !bounded_text(source, 128, false)
    {
        return Err(super::service::invalid());
    }
    let id = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES (?,?,?,?,?,?)",
        params![id, now, severity.as_str(), title, body, source],
    )?;
    retain(c, now)?;
    Ok(id)
}
pub(crate) fn retain(c: &Connection, now: i64) -> Result<()> {
    c.execute("DELETE FROM notifications WHERE created_at<? OR cursor IN (SELECT cursor FROM notifications ORDER BY cursor DESC LIMIT -1 OFFSET ?)",params![now.saturating_sub(HISTORY_MS),MAX_NOTIFICATIONS])?;
    Ok(())
}
