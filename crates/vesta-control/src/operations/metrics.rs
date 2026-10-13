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
