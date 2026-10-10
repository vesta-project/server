use super::{
    BuiltinRole, Capability, DeviceId, PrincipalId, Resource, ResourceScope, RoleGrant, RoleId,
};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DenyReason {
    NoIdentity,
    LocalOnly,
    InvalidResource,
    ExplicitDeny,
    Reserved,
    NotGranted,
    Rank,
    SelfTarget,
    NotDelegable,
    LocalGrantOnly,
    AdminReserved,
    OwnerReserved,
}
impl DenyReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoIdentity => "no_identity",
            Self::LocalOnly => "local_only",
            Self::InvalidResource => "invalid_resource",
            Self::ExplicitDeny => "explicit_deny",
            Self::Reserved => "reserved",
            Self::NotGranted => "not_granted",
            Self::Rank => "rank",
            Self::SelfTarget => "self",
            Self::NotDelegable => "not_delegable",
            Self::LocalGrantOnly => "local_grant_only",
            Self::AdminReserved => "admin_reserved",
            Self::OwnerReserved => "owner_reserved",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
}
impl Decision {
    pub const fn allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Allow => "allowed",
            Self::Deny(reason) => reason.code(),
        }
    }
}

/// A private-field observation, not a reusable mutation authority or authenticated
/// session. Commands NEVER accept a snapshot from the caller. They reload current
/// authority inside their write transaction. No grant-taking constructor exists.
///
/// The fields are private to this module and the only constructor that accepts grants is
/// `pub(super)`, so an external caller cannot build a snapshot at all and nothing outside
/// `identity` — production module or test module — can supply one. Inside `identity` the
/// constructor is a convention, not a language-level barrier; `repository.rs` is its only
/// production caller.
#[derive(Clone)]
pub struct PolicySnapshot {
    principal: Option<PrincipalId>,
    device: Option<DeviceId>,
    rank: i32,
    roles: Vec<RoleId>,
    builtins: HashSet<BuiltinRole>,
    allows: Vec<RoleGrant>,
    denies: Vec<RoleGrant>,
}

/// Redacted on purpose: an effective snapshot discloses live capability grants, which must
/// not reach a log line or a panic message.
impl std::fmt::Debug for PolicySnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PolicySnapshot")
            .field("has_current_identity", &self.has_current_identity())
            .field("rank", &self.rank)
            .field("role_count", &self.roles.len())
            .field("builtin_roles", &self.builtins)
            .field("allow_count", &self.allows.len())
            .field("deny_count", &self.denies.len())
            .finish()
    }
}

impl PolicySnapshot {
    /// The single construction path that accepts grants. Called only by `repository.rs`,
    /// after it has read current authority inside the caller's own transaction.
    pub(super) fn from_resolved(
        principal: Option<PrincipalId>,
        device: Option<DeviceId>,
        rank: i32,
        roles: Vec<RoleId>,
        builtins: HashSet<BuiltinRole>,
        allows: Vec<RoleGrant>,
        denies: Vec<RoleGrant>,
    ) -> Self {
        Self {
            principal,
            device,
            rank,
            roles,
            builtins,
            allows,
            denies,
        }
    }
    pub fn none() -> Self {
        Self::from_resolved(
            None,
            None,
            0,
            Vec::new(),
            HashSet::new(),
            Vec::new(),
            Vec::new(),
        )
    }
    pub fn has_current_identity(&self) -> bool {
        self.principal.is_some() && self.device.is_some()
    }
    pub fn principal(&self) -> Option<&PrincipalId> {
        self.principal.as_ref()
    }
    pub fn device(&self) -> Option<&DeviceId> {
        self.device.as_ref()
    }
    pub const fn rank(&self) -> i32 {
        self.rank
    }
    pub fn roles(&self) -> &[RoleId] {
        &self.roles
    }
    pub fn holds_builtin(&self, role: BuiltinRole) -> bool {
        self.builtins.contains(&role)
    }
    fn tier_permits(&self, capability: Capability) -> bool {
        if capability.local_only() {
            return false;
        }
        if capability.owner_reserved() {
            return self.holds_builtin(BuiltinRole::Owner);
        }
        if capability.admin_reserved() {
            return self.holds_builtin(BuiltinRole::Owner)
                || self.holds_builtin(BuiltinRole::Administrator);
        }
        true
    }
    /// A scoped check requires an explicit Resource (ROOT is valid). A global
    /// capability requires None. No raw path normalization or file I/O occurs.
    pub fn check(&self, capability: Capability, resource: Option<&Resource>) -> Decision {
        if !self.has_current_identity() {
            return Decision::Deny(DenyReason::NoIdentity);
        }
        if capability.local_only() {
            return Decision::Deny(DenyReason::LocalOnly);
        }
        if capability.scoped() != resource.is_some() {
            return Decision::Deny(DenyReason::InvalidResource);
        }
        if self.denies.iter().any(|grant| {
            grant.capability() == capability
                && resource.is_none_or(|resource| grant.scope().denies(resource))
        }) {
            return Decision::Deny(DenyReason::ExplicitDeny);
        }
        if !self.tier_permits(capability) {
            return Decision::Deny(DenyReason::Reserved);
        }
        if self.allows.iter().any(|grant| {
            grant.capability() == capability
                && resource.is_none_or(|resource| grant.scope().allows(resource))
        }) {
            return Decision::Allow;
        }
        Decision::Deny(DenyReason::NotGranted)
    }
    /// Surface admission only, before a server-resolved resource is known. Every
    /// actual resource still requires `check`; this is never an effect grant.
    pub(crate) fn has_scoped_grant(&self, capability: Capability) -> bool {
        self.has_current_identity()
            && capability.scoped()
            && self.tier_permits(capability)
            && self.allows.iter().any(|g| g.capability() == capability)
            && !self
                .denies
                .iter()
                .any(|g| g.capability() == capability && g.scope().is_all())
    }
    /// Network admission before a server-resolved leaf name is known. Establishes
    /// only that at least one direct child with this suffix is currently writable;
    /// the resolved name must still pass the ordinary exact-resource check.
    pub(crate) fn check_any_direct_child(
        &self,
        capability: Capability,
        directory: &Resource,
        suffix: &str,
    ) -> Decision {
        if !self.has_current_identity() {
            return Decision::Deny(DenyReason::NoIdentity);
        }
        let prefix = if directory.is_root() {
            String::new()
        } else {
            format!("{}/", directory.as_str())
        };
        for grant in self.allows.iter().filter(|g| g.capability() == capability) {
            let text = grant.scope().text();
            // Exact leaf grants must not be lost by checking only the directory.
            if let Some(leaf) = text.strip_prefix(&prefix)
                && !leaf.contains('/')
                && leaf.to_ascii_lowercase().ends_with(suffix)
                && let Ok(resource) = Resource::parse(&text)
                && self.check(capability, Some(&resource)).allowed()
            {
                return Decision::Allow;
            }
        }
        // A broad grant permits every child. Each non-covering deny can rule out
        // at most one of these distinct direct-child witnesses; N+1 is sufficient.
        // Covering subtree/all denies will refuse every witness through `check`.
        for index in 0..=self.denies.len() {
            let Ok(resource) = Resource::parse(&format!("{prefix}vesta-admission-{index}{suffix}"))
            else {
                return Decision::Deny(DenyReason::InvalidResource);
            };
            if self.check(capability, Some(&resource)).allowed() {
                return Decision::Allow;
            }
        }
        Decision::Deny(DenyReason::NotGranted)
    }

    pub fn holds_entirely(&self, capability: Capability, scope: &ResourceScope) -> bool {
        if !self.has_current_identity()
            || !self.tier_permits(capability)
            || (!capability.scoped() && !scope.is_all())
        {
            return false;
        }
        if self.denies.iter().any(|grant| {
            grant.capability() == capability
                && (!capability.scoped() || grant.scope().overlaps(scope))
        }) {
            return false;
        }
        self.allows.iter().any(|grant| {
            grant.capability() == capability
                && (!capability.scoped() || grant.scope().covers(scope))
        })
    }
}
