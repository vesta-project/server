use std::{error::Error, fmt, io};

/// Stable, input-free state failure. Native/SQL error text is deliberately not retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateError {
    code: &'static str,
}

impl StateError {
    /// Codes are trusted program constants, never caller text. A bare domain code is a bug.
    pub const fn new(code: &'static str) -> Self {
        assert!(
            code.len() > 6 && matches!(code.as_bytes(), [b's', b't', b'a', b't', b'e', b'_', ..]),
            "StateError requires a state_ code"
        );
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Do not expose SQLite messages, file paths, SQL, or callback inputs.
        f.write_str("State operation failed")
    }
}

impl Error for StateError {}

impl From<rusqlite::Error> for StateError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("state_io")
    }
}

impl From<io::Error> for StateError {
    fn from(_: io::Error) -> Self {
        Self::new("state_io")
    }
}

#[cfg(test)]
mod tests {
    use super::StateError;

    #[test]
    fn state_error_namespace_is_a_constructor_invariant() {
        const VALID: StateError = StateError::new("state_fixture");
        assert_eq!(VALID.code(), "state_fixture");
        for bare in [
            "",
            "state_",
            "not_found",
            "stale_revision",
            "xstate_failure",
        ] {
            assert!(std::panic::catch_unwind(|| StateError::new(bare)).is_err());
        }
    }
}

/// Keeps a repository's typed rejection separate from storage and rollback failures.
/// Formatting never invokes the application error's Display or Debug implementation.
#[derive(Clone, Eq, PartialEq)]
pub enum TransactionError<E> {
    State(StateError),
    Work(E),
    Rollback {
        primary: Box<Self>,
        failure: StateError,
    },
}

impl<E> From<StateError> for TransactionError<E> {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}

impl TransactionError<StateError> {
    pub(crate) fn into_state(self) -> StateError {
        match self {
            Self::State(error) | Self::Work(error) => error,
            Self::Rollback { failure, .. } => failure,
        }
    }
}

impl<E> fmt::Display for TransactionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("State transaction failed")
    }
}

impl<E> fmt::Debug for TransactionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => formatter.debug_tuple("State").field(error).finish(),
            Self::Work(_) => formatter.write_str("Work(<redacted>)"),
            Self::Rollback { primary, failure } => formatter
                .debug_struct("Rollback")
                .field("primary", primary)
                .field("failure", failure)
                .finish(),
        }
    }
}

impl<E: 'static> Error for TransactionError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            // The application owns E's disclosure policy; don't expose it through a
            // generic error formatter or require E to implement Error.
            Self::Work(_) => None,
            Self::Rollback { primary, .. } => Some(primary.as_ref()),
        }
    }
}
