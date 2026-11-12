//! Private domain decisions, not a connection-taking public authorization service.
use super::{
    BuiltinRole, Capability, CustomRole, Decision, DenyReason, Effect, PolicySnapshot, PrincipalId,
    RoleGrant, RoleId,
};

pub(super) struct Role {
    pub(super) id: RoleId,
    pub(super) rank: i32,
    pub(super) builtin: Option<BuiltinRole>,
    pub(super) grants: Vec<RoleGrant>,
}

fn lower_target(actor: &PolicySnapshot, target: &PrincipalId, rank: i32) -> Decision {
    if !actor.has_current_identity() {
        return Decision::Deny(DenyReason::NoIdentity);
    }
    if actor.principal() == Some(target) {
        return Decision::Deny(DenyReason::SelfTarget);
    }
    if rank >= actor.rank() {
        return Decision::Deny(DenyReason::Rank);
    }
    Decision::Allow
}

pub(super) fn assign(
    actor: &PolicySnapshot,
    target: &PrincipalId,
    target_rank: i32,
    role: &Role,
) -> Decision {
    let base = actor.check(Capability::RolesAssign, None);
    if !base.allowed() {
        return base;
    }
    if role.builtin.is_some_and(BuiltinRole::local_grant_only) {
        return Decision::Deny(DenyReason::LocalGrantOnly);
    }
    if role.rank >= actor.rank() {
        return Decision::Deny(DenyReason::Rank);
    }
    let target = lower_target(actor, target, target_rank);
    if !target.allowed() {
        return target;
    }
    within(actor, &role.grants)
}

pub(super) fn override_grant(
    actor: &PolicySnapshot,
    target: &PrincipalId,
    target_rank: i32,
    grant: &RoleGrant,
    effect: Effect,
) -> Decision {
    let base = actor.check(Capability::RolesAssign, None);
    if !base.allowed() {
        return base;
    }
    if grant.capability().local_only() {
        return Decision::Deny(DenyReason::LocalOnly);
    }
    let target = lower_target(actor, target, target_rank);
    if !target.allowed() {
        return target;
    }
    match effect {
        Effect::Deny => Decision::Allow,
        Effect::Allow => within(actor, std::slice::from_ref(grant)),
    }
}

pub(super) fn revoke(actor: &PolicySnapshot, target: &PrincipalId, target_rank: i32) -> Decision {
    let base = actor.check(Capability::RolesRevoke, None);
    if base.allowed() {
        lower_target(actor, target, target_rank)
    } else {
        base
    }
}

pub(super) fn revoke_device(
    actor: &PolicySnapshot,
    target: &PrincipalId,
    target_rank: i32,
) -> Decision {
    let base = actor.check(Capability::DevicesRevoke, None);
    if !base.allowed() {
        return base;
    } // explicit deny still wins for Owner
    if actor.principal() == Some(target) {
        return Decision::Allow;
    }
    lower_target(actor, target, target_rank)
}

pub(super) fn create(actor: &PolicySnapshot, role: &CustomRole) -> Decision {
    let base = actor.check(Capability::RolesCreate, None);
    if !base.allowed() {
        return base;
    }
    if role.rank().value() >= actor.rank() {
        return Decision::Deny(DenyReason::Rank);
    }
    within(actor, role.grants())
}

fn within(actor: &PolicySnapshot, grants: &[RoleGrant]) -> Decision {
    for grant in grants {
        let capability = grant.capability();
        if capability.local_only() {
            return Decision::Deny(DenyReason::LocalOnly);
        }
        if capability.owner_reserved() {
            return Decision::Deny(DenyReason::OwnerReserved);
        }
        if capability.admin_reserved() {
            return Decision::Deny(DenyReason::AdminReserved);
        }
        if !actor.holds_entirely(capability, grant.scope()) {
            return Decision::Deny(DenyReason::NotDelegable);
        }
    }
    Decision::Allow
}
