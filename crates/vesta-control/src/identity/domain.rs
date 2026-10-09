//! Validated application values. Persistence and wire parsing are not authority.
use super::{BuiltinRole, Capability, ResourceScope};
use crate::{ControlError, IdentityErrorCode, Result};
use std::{
    collections::BTreeSet,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn invalid() -> ControlError {
    ControlError::rejected(IdentityErrorCode::InvalidRequest)
}

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);
        impl $name {
            pub fn parse(value: &str) -> Result<Self> {
                if value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                {
                    return Err(invalid());
                }
                Ok(Self(value.into()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
identifier!(PrincipalId);
identifier!(DeviceId);
identifier!(RoleId);
identifier!(AssignmentId);
identifier!(OverrideId);

impl From<BuiltinRole> for RoleId {
    fn from(value: BuiltinRole) -> Self {
        Self(value.id().into())
    }
}

/// UTF-8 byte and Unicode-scalar bounds; no trimming, normalization or JDK text rules.
pub(super) fn valid_label(value: &str, max_scalars: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_scalars * 4
        && value.chars().count() <= max_scalars
        && !value.chars().all(char::is_whitespace)
        && !value.chars().any(char::is_control)
}

macro_rules! label {
    ($name:ident, $max:expr) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name(String);
        impl $name {
            pub fn parse(value: &str) -> Result<Self> {
                if !valid_label(value, $max) {
                    return Err(invalid());
                }
                Ok(Self(value.into()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}
label!(DisplayName, 64);
label!(DeviceLabel, 64);
label!(RoleName, 32);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Fingerprint(String);
impl Fingerprint {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Structural custom-role ceiling: never the locally granted administrator tier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CustomRank(u8);
impl CustomRank {
    pub fn new(rank: i32) -> Result<Self> {
        if !(1..BuiltinRole::Administrator.rank()).contains(&rank) {
            return Err(invalid());
        }
        Ok(Self(rank as u8))
    }
    pub const fn value(self) -> i32 {
        self.0 as i32
    }
}

/// Lookup keys only. A trusted transport must bind both to its authenticated device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subject {
    pub principal: PrincipalId,
    pub device: DeviceId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Allow,
    Deny,
}
impl Effect {
    pub(super) const fn stored(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// Relative expiry starts inside the mutation transaction, not at command construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expiry {
    Never,
    After(Duration),
}
impl Expiry {
    pub(super) fn deadline(self, now: i64) -> Result<Option<i64>> {
        match self {
            Self::Never => Ok(None),
            Self::After(duration) => {
                let ms = duration_millis(duration)?;
                if ms == 0 {
                    return Err(invalid());
                }
                Ok(Some(now.checked_add(ms).ok_or_else(invalid)?))
            }
        }
    }
}

pub const DEFAULT_CODE_LIFETIME: Duration = Duration::from_secs(600);
pub const MIN_CODE_LIFETIME: Duration = Duration::from_secs(60);
pub const MAX_CODE_LIFETIME: Duration = Duration::from_secs(3_600);
pub const MAX_PROOF_LIFETIME: Duration = Duration::from_secs(120);
pub const PROOF_CLOCK_SKEW: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeLifetime(Duration);
impl Default for CodeLifetime {
    fn default() -> Self {
        Self(DEFAULT_CODE_LIFETIME)
    }
}
impl CodeLifetime {
    pub fn new(duration: Duration) -> Result<Self> {
        duration_millis(duration)?;
        if !(MIN_CODE_LIFETIME..=MAX_CODE_LIFETIME).contains(&duration) {
            return Err(invalid());
        }
        Ok(Self(duration))
    }
    pub const fn duration(self) -> Duration {
        self.0
    }
    pub(super) fn deadline(self, now: i64) -> Result<i64> {
        now.checked_add(duration_millis(self.0)?)
            .ok_or_else(invalid)
    }
}

/// The database has integer-millisecond precision. Reject rather than round/truncate.
pub(super) fn duration_millis(duration: Duration) -> Result<i64> {
    // A negative interval has no meaning as a lifetime or relative expiry, and
    // `Duration::as_millis` does not report it as a negative number.
    if duration < Duration::ZERO || !duration.subsec_nanos().is_multiple_of(1_000_000) {
        return Err(invalid());
    }
    i64::try_from(duration.as_millis()).map_err(|_| invalid())
}

pub(super) fn system_time(ms: i64) -> Result<SystemTime> {
    let duration = Duration::from_millis(ms.unsigned_abs());
    let time = if ms >= 0 {
        UNIX_EPOCH.checked_add(duration)
    } else {
        UNIX_EPOCH.checked_sub(duration)
    };
    time.ok_or_else(|| vesta_state::StateError::new("state_identity_invalid").into())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleGrant {
    capability: Capability,
    scope: ResourceScope,
}
impl RoleGrant {
    pub fn new(capability: Capability, scope: ResourceScope) -> Result<Self> {
        if !capability.scoped() && !scope.is_all() {
            return Err(invalid());
        }
        Ok(Self { capability, scope })
    }
    pub const fn capability(&self) -> Capability {
        self.capability
    }
    pub fn scope(&self) -> &ResourceScope {
        &self.scope
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomRole {
    name: RoleName,
    rank: CustomRank,
    grants: Vec<RoleGrant>,
}
impl CustomRole {
    pub fn new(name: RoleName, rank: CustomRank, grants: Vec<RoleGrant>) -> Result<Self> {
        let mut capabilities = BTreeSet::new();
        if grants.is_empty()
            || grants.len() > Capability::ALL.len()
            || grants
                .iter()
                .any(|grant| !capabilities.insert(grant.capability()))
        {
            return Err(invalid());
        }
        Ok(Self { name, rank, grants })
    }
    pub fn name(&self) -> &RoleName {
        &self.name
    }
    pub const fn rank(&self) -> CustomRank {
        self.rank
    }
    pub fn grants(&self) -> &[RoleGrant] {
        &self.grants
    }
}

/// Commands name identity effects, never SQL, caller-selected audit text or authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteCommand {
    AssignRole {
        principal: PrincipalId,
        role: RoleId,
        expiry: Expiry,
    },
    SetOverride {
        principal: PrincipalId,
        grant: RoleGrant,
        effect: Effect,
        expiry: Expiry,
    },
    RevokeAssignment(AssignmentId),
    RevokeOverride(OverrideId),
    RevokeDevice(DeviceId),
    CreateRole(CustomRole),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    KeepAccess,
    RevokeAdministrativeAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCommand {
    Bootstrap(CodeLifetime),
    Recover {
        lifetime: CodeLifetime,
        action: RecoveryAction,
    },
    AssignRole {
        principal: PrincipalId,
        role: RoleId,
        expiry: Expiry,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityChange {
    Assignment(AssignmentId),
    Override(OverrideId),
    Role(RoleId),
    Revoked,
}
