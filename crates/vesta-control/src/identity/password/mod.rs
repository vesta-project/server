//! Bounded Argon2id primitives and process-local admission, not an authentication adapter.
//!
//! A pack password is an alternative to the normal Microsoft/Minecraft identity flow,
//! never administrative authority. Hashing and verification prove no device identity,
//! issue no grant, and own no database transaction. The application must authorize
//! management from current canonical policy and atomically audit its own effects.
//! Production composition must share one `KdfGate` across password services; cloning
//! shares its budgets. The primitive never creates a hidden budget or expensive-work queue.
mod gate;
mod hash;
mod secret;

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
pub(crate) use hash::test_work_counts;

pub use gate::{GateConfig, GateError, KdfGate, KdfPermit, Purpose};
pub use hash::{HashPolicy, KdfCost, MAX_ENCODED_LENGTH, PasswordHasher};
pub use secret::{MAX_PASSWORD_BYTES, MAX_PASSWORD_SCALARS, PasswordVerifier, SecretPassword};

/// Fixed, material-free errors. Native crypto, allocation and randomness errors are
/// erased at their boundary; no password, salt, verifier or engine message is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordError {
    InvalidPassword,
    InvalidPolicy,
    MalformedVerifier,
    UnsupportedVerifier,
    CostOutOfBounds,
    RandomUnavailable,
    KdfUnavailable,
}

impl PasswordError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidPassword => "invalid_password",
            Self::InvalidPolicy => "invalid_password_policy",
            Self::MalformedVerifier => "malformed_password_verifier",
            Self::UnsupportedVerifier => "unsupported_password_verifier",
            Self::CostOutOfBounds => "password_cost_out_of_bounds",
            Self::RandomUnavailable => "password_random_unavailable",
            Self::KdfUnavailable => "password_kdf_unavailable",
        }
    }
}

impl std::fmt::Display for PasswordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Password operation failed")
    }
}

impl std::error::Error for PasswordError {}
