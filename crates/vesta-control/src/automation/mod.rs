//! Durable local workflows and the per-principal notification inbox.
//!
//! Trusted hosts call `publish`, `tick`, and `notify`; transports must never expose
//! those system entry points without their own admission. Caller-facing methods
//! reload identity policy in the transaction that reads or changes state.
//!
//! Duplicate triggers are skipped while a workflow is active. Bounded catch-up
//! explicitly queues at most ten occurrences, executed serially. Suspended runs
//! block further executions until an authorized manager discards them. Definitions
//! and principals are copied into each run, so edits cannot rewrite running work.
//! A timeout records failure but does not cancel the host operation. Only explicit
//! failed outcomes may retry; uncertain outcomes and timeouts never retry.
//! Deadlines are checked before Delay wake completion too. Only explicit forbidden
//! authorization suspends; busy/unavailable stays pending once per pulse within
//! the original step deadline, while unsupported/invalid follows the failure branch.
//!
//! Ports must be non-blocking and must durably deduplicate submission keys.
//! Callbacks run outside database transactions and may access the same database;
//! stale callback results are rejected by optimistic state comparison. A committed
//! `submitting` intent is reconciled by the same key after a crash; a persisted
//! handle is only polled, never submitted. Hosts should have one runtime tick
//! driver; simultaneous drivers remain safe through idempotent intent keys and
//! atomic state comparisons, not through holding a database lock around a host.
//!
//! Retention is ninety days, at most 1,024 finished runs, 2,048 total runs,
//! 128 workflows and 4,096 notifications. Active/suspended work is never evicted;
//! capacity pressure skips new occurrences and records a notification.
mod cron;
mod inbox;
pub(crate) use inbox::insert as insert_notification;
mod model;
mod receipt;
mod service;

pub use cron::Cron;
pub use inbox::{Notification, NotificationPage};
pub use model::*;
pub use receipt::{Mutation, MutationError, MutationOutcome};
pub use service::{Automation, Run, RunStep, Workflow};

use crate::identity::PrincipalId;

pub trait ActionPort: Send + Sync {
    /// Current authority, including scopes, expiry and preset approval. Called
    /// before submitting every mutating step and again while reconciling it.
    fn authorize(&self, principal: &PrincipalId, action: &Action) -> Result<(), ActionError>;
    /// Non-blocking and durably idempotent per key, including crash reconciliation.
    fn begin(
        &self,
        principal: &PrincipalId,
        action: &Action,
        idempotency_key: &str,
    ) -> Result<ActionHandle, ActionError>;
    fn poll(&self, handle: &ActionHandle) -> Result<ActionStatus, ActionError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionHandle(pub String);
#[derive(Clone, Debug, PartialEq)]
pub enum ActionStatus {
    Running,
    Succeeded { output: serde_json::Value },
    Failed { code: String },
    Uncertain { code: String },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionError {
    Forbidden,
    Unsupported,
    Busy,
    Invalid,
    Unavailable,
}
impl ActionError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
            Self::Invalid => "invalid",
            Self::Unavailable => "unavailable",
        }
    }
}

#[cfg(test)]
mod tests;
