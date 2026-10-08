use super::{
    AssignmentId, DeviceId, DeviceLabel, DisplayName, EpochMillis, Fingerprint, PrincipalId,
    Purpose,
};
use crate::{ControlError, IdentityErrorCode, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{fmt, time::SystemTime};

/// An exact canonical 20-byte Base64URL token. No Java trimming or text aliases.
/// This checks syntax only; server binding, purpose, expiry and use are transactional.
pub struct CodeInput(String);
impl CodeInput {
    pub fn parse(value: &str) -> Result<Self> {
        let invalid = || ControlError::rejected(IdentityErrorCode::CodeInvalid);
        if value.len() != 27 {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid())?;
        if bytes.len() != 20 || URL_SAFE_NO_PAD.encode(&bytes) != value {
            return Err(invalid());
        }
        Ok(Self(value.into()))
    }
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for CodeInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodeInput[<redacted>]")
    }
}

/// Returned once to the trusted caller; only its digest is persisted. No serializer.
pub struct BootstrapCode {
    pub(super) code: String,
    pub(super) purpose: Purpose,
    pub(super) expires_at_ms: EpochMillis,
    pub(super) expires_at: SystemTime,
}
impl BootstrapCode {
    pub fn expose_secret(&self) -> &str {
        &self.code
    }
    pub const fn purpose(&self) -> Purpose {
        self.purpose
    }
    /// Persistence precision. Presentation uses `expires_at`; no JDK `Instant` text.
    pub const fn expires_at_ms(&self) -> EpochMillis {
        self.expires_at_ms
    }
    pub const fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
}
impl fmt::Debug for BootstrapCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootstrapCode")
            .field("purpose", &self.purpose)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("code", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub struct RedeemOwner {
    pub code: CodeInput,
    pub name: DisplayName,
    pub device_label: DeviceLabel,
    pub fingerprint: Fingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerAdded {
    pub principal: PrincipalId,
    pub device: DeviceId,
    pub assignment: AssignmentId,
}

#[derive(Debug)]
pub enum LocalOutcome {
    Code(BootstrapCode),
    Assignment(AssignmentId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrincipalStatus {
    Active,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Principal {
    pub id: PrincipalId,
    pub name: DisplayName,
    pub status: PrincipalStatus,
    pub created_at: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    pub principal: PrincipalId,
    pub label: DeviceLabel,
    pub fingerprint: Fingerprint,
    pub created_at: SystemTime,
    pub revoked_at: Option<SystemTime>,
}
impl Device {
    pub fn revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubjectRecord {
    pub principal: Principal,
    pub devices: Vec<Device>,
}
