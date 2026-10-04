package com.vesta.wrapper.authorization;

import com.vesta.wrapper.files.ServerPathPolicy;

import java.util.List;
import java.util.Objects;
import java.util.Set;

/**
 * Immutable effective remote policy for one authenticated principal+device at one instant.
 * Only {@link AuthorizationService} constructs non-empty policies; {@link #NONE} denies everything.
 *
 * <p>Hard invariants enforced at evaluation, independent of stored grants (so a damaged, migrated or
 * imported override/custom-role row cannot widen authority):
 * <ul>
 *   <li>local-only capabilities are never effective remotely;</li>
 *   <li>owner-reserved capabilities require active membership of the built-in Owner role;</li>
 *   <li>admin-reserved capabilities require active membership of built-in Owner or Administrator.</li>
 * </ul>
 * Membership comes from active assignments to roles flagged {@code builtin = 1} (immutable, unique ids),
 * never from numeric rank. An explicit deny still wins over everything.
 */
public final class EffectivePolicy {
    /** Grant row after validation: capability + canonical scope. */
    record Grant(Capability capability, ResourceScope scope) {
    }

    public static final String RESERVED = "reserved";

    public static final EffectivePolicy NONE =
            new EffectivePolicy(null, null, 0, List.of(), Set.of(), List.of(), List.of());

    private final String principalId;
    private final String deviceId;
    private final int rank;
    private final List<String> roleIds;
    private final Set<String> builtinRoleIds;
    private final List<Grant> allows;
    private final List<Grant> denies;

    EffectivePolicy(String principalId, String deviceId, int rank, List<String> roleIds,
                    Set<String> builtinRoleIds, List<Grant> allows, List<Grant> denies) {
        this.principalId = principalId;
        this.deviceId = deviceId;
        this.rank = rank;
        this.roleIds = List.copyOf(roleIds);
        this.builtinRoleIds = Set.copyOf(builtinRoleIds);
        this.allows = List.copyOf(allows);
        this.denies = List.copyOf(denies);
    }

    /** True when the principal actively holds the built-in role {@code roleId} (builtin flag verified). */
    public boolean holdsBuiltinRole(String roleId) {
        return builtinRoleIds.contains(roleId);
    }

    /** Tier gate shared by {@link #check} and {@link #holdsEntirely}; independent of stored grants. */
    private boolean tierPermits(Capability capability) {
        if (capability.localOnly()) {
            return false;
        }
        if (capability.ownerReserved()) {
            return holdsBuiltinRole(BuiltinRoles.OWNER);
        }
        if (capability.adminReserved()) {
            return holdsBuiltinRole(BuiltinRoles.OWNER) || holdsBuiltinRole(BuiltinRoles.ADMINISTRATOR);
        }
        return true;
    }

    public boolean isAuthenticated() {
        return principalId != null;
    }

    /** Null for {@link #NONE}. */
    public String principalId() {
        return principalId;
    }

    /** Null for {@link #NONE}. */
    public String deviceId() {
        return deviceId;
    }

    /** Highest active role rank; 0 when the principal holds no active role. */
    public int rank() {
        return rank;
    }

    public List<String> roleIds() {
        return roleIds;
    }

    /**
     * Checks one capability on one resource. Scoped (file) capabilities require a server-relative path
     * ({@code ""} = root); for unscoped capabilities the resource is ignored. Callers must check the exact
     * action's capability; there is no group fallback.
     *
     * <p>Scope matching here is lexical only. An allowed scope does NOT make a path safe to touch: the file
     * adapter must still confine it with {@link ServerPathPolicy#resolve} (protected internals, links,
     * root escape) before any I/O.
     */
    public Decision check(Capability capability, String resource) {
        Objects.requireNonNull(capability, "capability");
        if (!isAuthenticated()) {
            return Decision.deny(Decision.NO_IDENTITY);
        }
        if (capability.localOnly()) {
            return Decision.deny(Decision.LOCAL_ONLY);
        }
        String normalized = null;
        if (capability.scoped()) {
            try {
                normalized = ServerPathPolicy.normalize(resource, true);
            } catch (SecurityException e) {
                return Decision.deny(Decision.INVALID_RESOURCE);
            }
        }
        for (Grant deny : denies) {
            if (deny.capability() == capability && (!capability.scoped() || deny.scope().denies(normalized))) {
                return Decision.deny(Decision.EXPLICIT_DENY);
            }
        }
        if (!tierPermits(capability)) {
            return Decision.deny(RESERVED); // stored grants can never confer reserved tiers
        }
        for (Grant allow : allows) {
            if (allow.capability() == capability && (!capability.scoped() || allow.scope().allows(normalized))) {
                return Decision.allow();
            }
        }
        return Decision.deny(Decision.NOT_GRANTED);
    }

    public boolean allows(Capability capability, String resource) {
        return check(capability, resource).allowed();
    }

    /**
     * True when this policy holds {@code capability} over the whole of {@code scope} with no overlapping
     * explicit deny. Used for delegation: a delegator can never hand out more than it holds.
     */
    public boolean holdsEntirely(Capability capability, ResourceScope scope) {
        if (!isAuthenticated() || !tierPermits(capability)) {
            return false;
        }
        for (Grant deny : denies) {
            if (deny.capability() == capability && (!capability.scoped() || deny.scope().overlaps(scope))) {
                return false;
            }
        }
        for (Grant allow : allows) {
            if (allow.capability() == capability && (!capability.scoped() || allow.scope().covers(scope))) {
                return true;
            }
        }
        return false;
    }
}
