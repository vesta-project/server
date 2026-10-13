//! Reads authenticate against fresh policy in the same snapshot as telemetry.
use super::*;
use crate::metrics::{self, Activity, Current, Range, Resolution};
impl Operations {
    fn metrics_read<T>(
        &self,
        subject: &Subject,
        capability: Capability,
        read: impl FnOnce(&rusqlite::Connection) -> vesta_state::Result<T>,
    ) -> OperationsResult<T> {
        self.db
            .read_with(|snapshot| {
                let repo = Repository::new(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                if let identity::Decision::Deny(reason) =
                    self.policy(&repo, subject)?.check(capability, None)
                {
                    return Err(OperationError::forbidden(reason));
                }
                read(snapshot.connection()).map_err(|e| {
                    if e.code() == "state_metrics_invalid_request" {
                        invalid()
                    } else {
                        e.into()
                    }
                })
            })
            .map_err(OperationError::from)
    }
    pub fn metrics_current(&self, subject: &Subject) -> OperationsResult<Current> {
        self.metrics_read(subject, Capability::MetricsView, metrics::storage::current)
    }
    pub fn metrics_range(
        &self,
        subject: &Subject,
        series: &str,
        from: i64,
        to: i64,
        resolution: Resolution,
    ) -> OperationsResult<Range> {
        if !metrics::SERIES.contains(&series)
            || from < 0
            || to < from
            || to.saturating_sub(from) > 730 * metrics::DAY
        {
            return Err(invalid());
        }
        self.metrics_read(subject, Capability::MetricsView, |c| {
            metrics::storage::range(c, series, from, to, resolution)
        })
    }
    pub fn player_activity(
        &self,
        subject: &Subject,
        after: i64,
        limit: usize,
    ) -> OperationsResult<Vec<Activity>> {
        if after < 0 || !(1..=101).contains(&limit) {
            return Err(invalid());
        }
        self.metrics_read(subject, Capability::PlayersView, |c| {
            metrics::storage::activity(c, after, limit)
        })
    }
}

impl Operations {
    pub fn metrics_policy(
        &self,
        subject: &Subject,
        fallback: crate::config::MetricsSettings,
    ) -> OperationsResult<metrics::Policy> {
        self.metrics_read(subject, Capability::ConfigView, |c| {
            metrics::get_policy(c, fallback)
        })
    }
    pub fn set_metrics_policy(
        &self,
        subject: &Subject,
        expected: Option<i64>,
        document: metrics::PolicyDocument,
    ) -> OperationsResult<i64> {
        if !document.valid() || expected.is_some_and(|r| r <= 0) {
            return Err(invalid());
        }
        let result = self.db.write_with(|transaction| {
            use rusqlite::OptionalExtension;
            let c = transaction.connection();
            let repo = Repository::new(c, self.db.server_id(), self.db.clock().millis());
            if let identity::Decision::Deny(reason) = self.policy(&repo, subject)?.check(Capability::ConfigEdit, None) { return Err(OperationError::forbidden(reason)); }
            let current: Option<i64> = c.query_row("SELECT revision FROM metrics_policy WHERE singleton=1", [], |r| r.get(0)).optional()?;
            if current != expected { return Err(OperationError::rejected(OperationErrorCode::SourceRevisionUnavailable)); }
            let revision = current.unwrap_or(0).checked_add(1).ok_or_else(invalid)?;
            let json = serde_json::to_string(&document).map_err(|_| invalid())?;
            c.execute("INSERT INTO metrics_policy(singleton,revision,document) VALUES(1,?,?) ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision,document=excluded.document", rusqlite::params![revision,json])?;
            // Sampling cadence and retention affect automatic resolution and
            // bucket widths even before the next sample. Invalidate validators
            // and tell incremental clients to rebuild their full projection.
            c.execute("UPDATE metrics_status SET revision=revision+1,prune_revision=revision+1 WHERE singleton=1", [])?;
            self.audit.append(c, &AuditRecord::new(request_actor(subject)?, "metrics.policy.update".into(), "metrics-policy".into(), "accepted".into(), None, [("revision".into(), Some(revision.to_string())), ("tick_polling".into(), Some(document.command_polling.enabled.to_string()))])?)?;
            Ok(revision)
        });
        self.finish_request(result, subject, "metrics.policy.update", None)
    }
}

impl Operations {
    pub fn metrics_fetch(
        &self,
        subject: &Subject,
        request: &metrics::Fetch,
        fallback: crate::config::MetricsSettings,
    ) -> OperationsResult<metrics::Fetched> {
        if !request.valid() {
            return Err(invalid());
        }
        self.metrics_read(subject, Capability::MetricsView, |c| {
            let (_, document) = metrics::effective_policy(c, fallback)?;
            metrics::fetch(c, request, document.settings)
        })
    }
}

/// One fresh permission snapshot for a live push. Player identities never escape
/// this read when PlayersView is absent. Slow consumers see at most 32 latest events.
pub struct MetricsLive {
    pub current: Current,
    pub activity: Vec<Activity>,
    pub activity_cursor: i64,
    pub activity_gap: bool,
}
impl Operations {
    pub fn metrics_live(
        &self,
        subject: &Subject,
        after: Option<i64>,
    ) -> OperationsResult<MetricsLive> {
        if after.is_some_and(|a| a < 0) {
            return Err(invalid());
        }
        self.metrics_read(subject, Capability::MetricsView, |c| {
            let repo=Repository::new(c,self.db.server_id(),self.db.clock().millis());
            let players=matches!(self.policy(&repo,subject).map_err(|_|vesta_state::StateError::new("state_metrics_unavailable"))?.check(Capability::PlayersView,None),identity::Decision::Allow);
            let cursor:i64=c.query_row("SELECT COALESCE(MAX(id),0) FROM metrics_activity",[],|r|r.get(0))?;
            let mut activity=Vec::new(); let mut gap=false;
            if players && let Some(after)=after {
                activity=c.prepare("SELECT id,at,generation,name,kind FROM metrics_activity WHERE id>? ORDER BY id DESC LIMIT 33")?.query_map([after],|r|Ok(Activity{id:r.get(0)?,at:r.get(1)?,generation:r.get(2)?,name:r.get(3)?,kind:r.get(4)?}))?.collect::<std::result::Result<_,_>>()?;
                gap=activity.len()>32;
                activity.truncate(32); activity.reverse();
                gap |= activity.first().is_some_and(|a|a.id>after.saturating_add(1));
            }
            Ok(MetricsLive {current:metrics::storage::current(c)?,activity,activity_cursor:cursor,activity_gap:players&&gap})
        })
    }
}
