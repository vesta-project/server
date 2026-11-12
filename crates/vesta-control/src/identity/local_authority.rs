//! Local authority remains closed until a separately reviewed OS-authenticated adapter exists.
//! No public proof constructor, parser, deserializer or verifier injection is exposed.
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Purpose {
    OwnerBootstrap,
    OwnerRecovery,
    LocalRoleGrant,
}

impl Purpose {
    pub const fn id(self) -> &'static str {
        match self {
            Self::OwnerBootstrap => "owner_bootstrap",
            Self::OwnerRecovery => "owner_recovery",
            Self::LocalRoleGrant => "local_role_grant",
        }
    }
}

/// Opaque channel evidence, not a caller-created authorization token. Production
/// currently has no issuer and refuses even a proof originating in unit tests.
pub struct LocalProof {
    _sealed: (),
    #[cfg(test)]
    attestation: Option<Attestation>,
}

impl fmt::Debug for LocalProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalProof[<redacted>]")
    }
}

#[cfg(test)]
impl LocalProof {
    pub(crate) fn for_test(payload: Attestation) -> Self {
        Self {
            _sealed: (),
            attestation: Some(payload),
        }
    }

    pub(crate) fn foreign_for_test() -> Self {
        Self {
            _sealed: (),
            attestation: None,
        }
    }

    pub(crate) fn attestation_for_test(&self) -> Option<Attestation> {
        self.attestation.clone()
    }
}

/// Never a wire object. Only a trusted authority may attest these values; the
/// identity service independently validates purpose, freshness, lifetime and replay.
#[derive(Clone)]
pub(crate) struct Attestation {
    pub(crate) nonce: String,
    pub(crate) purpose: Purpose,
    pub(crate) os_actor: String,
    pub(crate) issued_at_ms: i64,
    pub(crate) expires_at_ms: i64,
}

impl fmt::Debug for Attestation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attestation")
            .field("purpose", &self.purpose)
            .field("os_actor", &self.os_actor)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("nonce", &"<redacted>")
            .finish()
    }
}

// Crate-private, with no production injection seam. A real adapter will require
// its own explicit security review; making this trait public is not that adapter.
pub(crate) trait LocalAuthority: Send + Sync {
    fn verify(&self, proof: &LocalProof, expected: Purpose) -> Option<Attestation>;
}

pub(super) struct UnavailableAuthority;

impl LocalAuthority for UnavailableAuthority {
    fn verify(&self, _proof: &LocalProof, _expected: Purpose) -> Option<Attestation> {
        None
    }
}
