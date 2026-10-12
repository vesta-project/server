//! Validated task values and input-free failures. None of these values is authority.
use crate::{ControlError, identity::DenyReason};
use std::{error::Error, fmt};
use vesta_game_runtime::process::RuntimeError;
use vesta_state::{StateError, TransactionError};

pub const MAX_PAGE: usize = 200;
pub(crate) const MAX_INPUT_BYTES: usize = 65_536;
pub(crate) const MAX_RESOURCES: usize = 32;
pub(crate) const MAX_REQUIREMENTS: usize = 32;
pub(crate) const MAX_LOCK_BYTES: usize = 256;
pub(crate) const REQUEST_SOURCE: &str = "operation-request";
pub(crate) const ENGINE_SOURCE: &str = "operation-kernel";
pub(crate) const MAX_RESULT_BYTES: usize = 16_384;

pub type OperationsResult<T> = std::result::Result<T, OperationError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationErrorCode {
    InvalidRequest,
    Forbidden,
    NotFound,
    ActorMismatch,
    UnknownType,
    InputUnavailable,
    BindingMismatch,
    IdempotencyConflict,
    InvalidTransition,
    NotOwner,
    RecoveryRequired,
    CancellationRequested,
    OwnerStopping,
    ConfirmationRejected,
    SourceRevisionUnavailable,
    ResultTooLarge,
    /// Maintenance such as a stopped-server backup forbids starting the game now.
    MaintenanceActive,
}
impl OperationErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::ActorMismatch => "actor_mismatch",
            Self::UnknownType => "unknown_type",
            Self::InputUnavailable => "input_unavailable",
            Self::BindingMismatch => "binding_mismatch",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::InvalidTransition => "invalid_transition",
            Self::NotOwner => "not_owner",
            Self::RecoveryRequired => "recovery_required",
            Self::CancellationRequested => "cancellation_requested",
            Self::OwnerStopping => "owner_stopping",
            Self::ConfirmationRejected => "confirmation_rejected",
            Self::SourceRevisionUnavailable => "source_revision_unavailable",
            Self::ResultTooLarge => "result_too_large",
            Self::MaintenanceActive => "maintenance_active",
        }
    }
}

/// Only trusted fixed codes/reasons are retained. A policy resolver failure is not
/// an operation refusal, and infrastructure/rollback failures must not be audited
/// as if authorization had returned an ordinary denial.
#[derive(Clone, Eq, PartialEq)]
pub enum OperationError {
    State(StateError),
    /// Native process failures preserve their own fixed namespace and cause;
    /// they are neither database failures nor ordinary authorization denials.
    Runtime(RuntimeError),
    Rejected {
        code: OperationErrorCode,
        reason: Option<DenyReason>,
    },
    Policy(ControlError),
    Rollback {
        primary: Box<OperationError>,
        failure: StateError,
    },
    AuditUnavailable {
        primary: Box<OperationError>,
        audit_failure: Box<OperationError>,
    },
}
impl OperationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::State(error) => error.code(),
            Self::Runtime(error) => error.code(),
            Self::Rejected { code, .. } => code.code(),
            Self::Policy(error) => error.code(),
            Self::Rollback { .. } => "rollback_failed",
            Self::AuditUnavailable { .. } => "audit_unavailable",
        }
    }
    pub const fn reason(&self) -> Option<DenyReason> {
        match self {
            Self::Rejected { reason, .. } => *reason,
            Self::Policy(error) => error.reason(),
            _ => None,
        }
    }
    pub const fn primary(&self) -> &Self {
        match self {
            Self::Rollback { primary, .. } | Self::AuditUnavailable { primary, .. } => {
                primary.primary()
            }
            other => other,
        }
    }
    pub fn secondary(&self) -> Option<StateError> {
        match self {
            Self::Rollback { failure, .. } => Some(*failure),
            Self::AuditUnavailable { audit_failure, .. } => match audit_failure.as_ref() {
                Self::State(error) => Some(*error),
                _ => None,
            },
            Self::Policy(error) => error.secondary(),
            _ => None,
        }
    }
    pub(crate) const fn rejected(code: OperationErrorCode) -> Self {
        Self::Rejected { code, reason: None }
    }
    pub(crate) const fn forbidden(reason: DenyReason) -> Self {
        Self::Rejected {
            code: OperationErrorCode::Forbidden,
            reason: Some(reason),
        }
    }
}
impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Operation request failed")
    }
}
impl fmt::Debug for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("OperationError");
        out.field("code", &self.code())
            .field("reason", &self.reason());
        match self {
            Self::Policy(error) => {
                out.field("policy_failure", error);
            }
            Self::Rollback { primary, failure } => {
                out.field("primary", primary)
                    .field("rollback_failure", &failure.code());
            }
            Self::AuditUnavailable {
                primary,
                audit_failure,
            } => {
                out.field("primary", primary)
                    .field("audit_failure", audit_failure);
            }
            _ => {}
        }
        out.finish()
    }
}
impl Error for OperationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Policy(error) => Some(error),
            Self::Rollback { primary, .. } | Self::AuditUnavailable { primary, .. } => {
                Some(primary.as_ref())
            }
            Self::Rejected { .. } => None,
        }
    }
}
impl From<StateError> for OperationError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
impl From<RuntimeError> for OperationError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}
impl From<rusqlite::Error> for OperationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::State(error.into())
    }
}
impl From<ControlError> for OperationError {
    fn from(error: ControlError) -> Self {
        Self::Policy(error)
    }
}
impl From<TransactionError<OperationError>> for OperationError {
    fn from(error: TransactionError<OperationError>) -> Self {
        match error {
            TransactionError::State(error) => Self::State(error),
            TransactionError::Work(error) => error,
            TransactionError::Rollback { primary, failure } => Self::Rollback {
                primary: Box::new(Self::from(*primary)),
                failure,
            },
        }
    }
}

pub(crate) fn corrupt() -> OperationError {
    StateError::new("state_operation_corrupt").into()
}
pub(crate) fn invalid() -> OperationError {
    OperationError::rejected(OperationErrorCode::InvalidRequest)
}
pub(crate) fn identifier(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
pub(crate) fn lock_key(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_LOCK_BYTES
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':'))
}
pub(crate) fn hash_text(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn text(text: &str, max_bytes: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_bytes
        && !text.chars().all(char::is_whitespace)
        && !text.chars().any(char::is_control)
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationId(String);
impl OperationId {
    pub fn parse(value: &str) -> OperationsResult<Self> {
        if !identifier(value, 64) {
            return Err(invalid());
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
    Queued,
    Validating,
    AwaitingConfirmation,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    RecoveryRequired,
}
impl OperationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Validating => "validating",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::RecoveryRequired => "recovery_required",
        }
    }
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
    pub(crate) const fn is_claimed(self) -> bool {
        matches!(self, Self::Validating | Self::Running | Self::Cancelling)
    }
    pub(crate) fn stored(value: &str) -> OperationsResult<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "validating" => Ok(Self::Validating),
            "awaiting_confirmation" => Ok(Self::AwaitingConfirmation),
            "running" => Ok(Self::Running),
            "cancelling" => Ok(Self::Cancelling),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(corrupt()),
        }
    }
}

/// Fixed lifecycle facts only. No stored free-form text or execution input is projected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationSummary {
    pub id: OperationId,
    pub seq: i64,
    pub state: OperationState,
    pub progress_percent: u8,
    pub cancel_requested: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationPage {
    pub items: Vec<OperationSummary>,
    pub next_after_seq: i64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CancelOutcome {
    Cancelled(OperationSummary),
    Requested(OperationSummary),
    AlreadyRequested(OperationSummary),
    AlreadyTerminal(OperationSummary),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Admission {
    Created(OperationSummary),
    Existing(OperationSummary),
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PurgeReport {
    pub(crate) purged: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_failure_preserves_native_type_and_fixed_redacted_errors() {
        let error = OperationError::from(RuntimeError::ObserveFailed);
        assert!(matches!(
            error,
            OperationError::Runtime(RuntimeError::ObserveFailed)
        ));
        assert_eq!(error.code(), "game_observe_failed");
        assert_eq!(error.reason(), None);
        assert_eq!(error.secondary(), None);
        assert_eq!(error.primary(), &error);
        assert_eq!(error.to_string(), "Operation request failed");
        assert_eq!(
            format!("{error:?}"),
            "OperationError { code: \"game_observe_failed\", reason: None }"
        );
        // The source is the fixed typed runtime enum, never a raw native IO/
        // process error or a forged StateError with a game_* database code.
        let source = error.source().expect("preserved typed runtime cause");
        assert_eq!(source.to_string(), "Game runtime failed");
        assert!(source.downcast_ref::<RuntimeError>().is_some());
        assert!(source.downcast_ref::<StateError>().is_none());
    }
}
