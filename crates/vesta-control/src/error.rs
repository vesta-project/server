use crate::identity::DenyReason;
use std::{error::Error, fmt};
use vesta_state::{StateError, TransactionError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityErrorCode {
    Forbidden,
    LocalAuthorityRequired,
    CodeInvalid,
    BootstrapClosed,
    NotFound,
    Conflict,
    InvalidRequest,
    RoleInvalid,
    CredentialConflict,
    CredentialRevisionExhausted,
    InvalidPassword,
    PasswordKdfUnavailable,
    PasswordRateLimited,
    PasswordKdfBusy,
}
impl IdentityErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::LocalAuthorityRequired => "local_authority_required",
            Self::CodeInvalid => "code_invalid",
            Self::BootstrapClosed => "bootstrap_closed",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::InvalidRequest => "invalid_request",
            Self::RoleInvalid => "role_invalid",
            Self::CredentialConflict => "credential_conflict",
            Self::CredentialRevisionExhausted => "credential_revision_exhausted",
            Self::InvalidPassword => "invalid_password",
            Self::PasswordKdfUnavailable => "password_kdf_unavailable",
            Self::PasswordRateLimited => "password_rate_limited",
            Self::PasswordKdfBusy => "password_kdf_busy",
        }
    }
}

/// Only safe fixed codes/reasons are retained. Native errors are erased immediately.
/// Transaction rollback and separate refusal-audit failures preserve their primaries.
#[derive(Clone, Eq, PartialEq)]
pub enum ControlError {
    State(StateError),
    Rejected {
        code: IdentityErrorCode,
        reason: Option<DenyReason>,
    },
    Rollback {
        primary: Box<ControlError>,
        failure: StateError,
    },
    AuditUnavailable {
        primary: Box<ControlError>,
        audit_failure: Box<ControlError>,
    },
}
impl ControlError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::State(error) => error.code(),
            Self::Rejected { code, .. } => code.code(),
            Self::Rollback { .. } => "rollback_failed",
            Self::AuditUnavailable { .. } => "audit_unavailable",
        }
    }
    pub const fn reason(&self) -> Option<DenyReason> {
        match self {
            Self::Rejected { reason, .. } => *reason,
            _ => None,
        }
    }
    /// The failure a rolled-back command actually produced. `Rollback` and
    /// `AuditUnavailable` preserve it, so a caller can still report why the work was
    /// refused even when a second failure has been layered on top.
    pub const fn primary(&self) -> &Self {
        match self {
            Self::Rollback { primary, .. } | Self::AuditUnavailable { primary, .. } => primary,
            other => other,
        }
    }
    /// The secondary infrastructure failure, when this error carries exactly one.
    pub fn secondary(&self) -> Option<StateError> {
        match self {
            Self::Rollback { failure, .. } => Some(*failure),
            Self::AuditUnavailable { audit_failure, .. } => match audit_failure.as_ref() {
                Self::State(error) => Some(*error),
                _ => None,
            },
            _ => None,
        }
    }
    pub(crate) const fn rejected(code: IdentityErrorCode) -> Self {
        Self::Rejected { code, reason: None }
    }
    pub(crate) const fn forbidden(reason: DenyReason) -> Self {
        Self::Rejected {
            code: IdentityErrorCode::Forbidden,
            reason: Some(reason),
        }
    }
}
impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Control operation failed")
    }
}
impl fmt::Debug for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("ControlError");
        debug
            .field("code", &self.code())
            .field("reason", &self.reason());
        match self {
            Self::Rollback { primary, failure } => {
                debug
                    .field("primary", primary)
                    .field("rollback_failure_code", &failure.code());
            }
            Self::AuditUnavailable {
                primary,
                audit_failure,
            } => {
                debug
                    .field("primary", primary)
                    .field("audit_failure", audit_failure);
            }
            _ => {}
        }
        debug.finish()
    }
}
impl Error for ControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            Self::Rollback { primary, .. } | Self::AuditUnavailable { primary, .. } => {
                Some(primary.as_ref())
            }
            Self::Rejected { .. } => None,
        }
    }
}
impl From<StateError> for ControlError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
impl From<rusqlite::Error> for ControlError {
    fn from(error: rusqlite::Error) -> Self {
        Self::State(error.into())
    }
}
impl From<TransactionError<ControlError>> for ControlError {
    fn from(error: TransactionError<ControlError>) -> Self {
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
