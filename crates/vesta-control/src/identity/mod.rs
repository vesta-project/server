//! Canonical server-local identity and deny-by-default policy.
//!
//! `Subject` identifiers are only lookup keys supplied by a future authenticated
//! device transport; accepting or constructing them does not authenticate a caller.
//! Every mutation reloads current policy inside its own write transaction, so no
//! caller can hand a reusable authority snapshot back into a command. Local proof
//! authority remains unavailable in production. Resource scopes are lexical, not
//! filesystem confinement, and naming a capability does not implement a handler.
mod authorization;
mod builtin_roles;
mod capability;
mod domain;
mod local_authority;
pub mod minecraft;
mod pack_passwords;
pub mod password;
mod policy;
mod records;
mod repository;
mod scope;
mod service;

pub use builtin_roles::BuiltinRole;
pub use capability::Capability;
pub use domain::{
    AssignmentId, AuthorityChange, CodeLifetime, CustomRank, CustomRole, DEFAULT_CODE_LIFETIME,
    DeviceId, DeviceLabel, DisplayName, Effect, Expiry, Fingerprint, LocalCommand,
    MAX_CODE_LIFETIME, MAX_PROOF_LIFETIME, MIN_CODE_LIFETIME, OverrideId, PROOF_CLOCK_SKEW,
    PrincipalId, RecoveryAction, RemoteCommand, RoleGrant, RoleId, RoleName, Subject,
};
pub use local_authority::{LocalProof, Purpose};
pub use pack_passwords::{
    PackPasswords, PasswordMetadata, PasswordRevision, PasswordVerification,
    PasswordVerificationStatus,
};
pub use policy::{Decision, DenyReason, PolicySnapshot};
pub use records::{
    BootstrapCode, CodeInput, Device, LocalOutcome, OwnerAdded, Principal, PrincipalStatus,
    RedeemOwner, SubjectRecord,
};
pub use scope::{Resource, ResourceScope};
pub use service::IdentityService;

pub type EpochMillis = i64;

#[cfg(test)]
mod tests;
