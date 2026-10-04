package com.vesta.wrapper.authorization;

import com.vesta.wrapper.state.StateDatabase;
import com.vesta.wrapper.state.StateException;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.Set;
import java.util.List;
import java.util.Objects;
import java.util.Optional;

/**
 * Canonical deny-by-default policy evaluator over the identity tables.
 *
 * <p>{@link #resolve} loads the <em>current</em> principal, device, role assignments and overrides on
 * every call; nothing is cached, so revocation, disablement and expiry apply to the very next check.
 * The principal/device identifiers must come from an authenticated transport (future mTLS/device
 * composition); this class does not authenticate anything and treats the ids only as lookup keys.
 */
public final class AuthorizationService {
    public static final String DENY_RANK = "rank";
    public static final String DENY_SELF = "self";
    public static final String DENY_NOT_DELEGABLE = "not_delegable";
    public static final String DENY_LOCAL_GRANT_ONLY = "local_grant_only";
    public static final String DENY_ADMIN_RESERVED = "admin_reserved";
    public static final String DENY_OWNER_RESERVED = "owner_reserved";

    /** A role with its validated capability grants. */
    public record RoleInfo(String id, String name, int rank, boolean builtin, List<RoleGrant> grants) {
        public RoleInfo {
            grants = List.copyOf(grants);
        }
    }

    public record RoleGrant(Capability capability, ResourceScope scope) {
    }

    private final StateDatabase db;

    public AuthorizationService(StateDatabase db) {
        this.db = Objects.requireNonNull(db, "db");
    }

    /** Resolves the current effective policy in its own read transaction. */
    public EffectivePolicy resolve(String principalId, String deviceId) {
        return db.read(c -> resolve(c, principalId, deviceId));
    }

    /**
     * Resolves inside the caller's transaction. Returns {@link EffectivePolicy#NONE} unless the principal
     * exists, is active and bound to this server, and the device exists, belongs to that principal, is
     * bound to this server and is not revoked.
     */
    public EffectivePolicy resolve(Connection c, String principalId, String deviceId) throws SQLException {
        if (!validId(principalId) || !validId(deviceId)) {
            return EffectivePolicy.NONE;
        }
        String serverId = db.serverId();
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT status, server_id FROM principals WHERE id = ?")) {
            ps.setString(1, principalId);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next() || !"active".equals(rs.getString(1)) || !serverId.equals(rs.getString(2))) {
                    return EffectivePolicy.NONE;
                }
            }
        }
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT principal_id, server_id, revoked_at FROM devices WHERE id = ?")) {
            ps.setString(1, deviceId);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next() || !principalId.equals(rs.getString(1)) || !serverId.equals(rs.getString(2))) {
                    return EffectivePolicy.NONE;
                }
                rs.getLong(3);
                if (!rs.wasNull()) {
                    return EffectivePolicy.NONE;
                }
            }
        }
        long now = db.clock().millis();
        int rank = 0;
        List<String> roleIds = new ArrayList<>();
        Set<String> builtinRoleIds = new HashSet<>();
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT r.id, r.rank, r.builtin FROM role_assignments a JOIN roles r ON r.id = a.role_id "
                        + "WHERE a.principal_id = ? AND a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at > ?)")) {
            ps.setString(1, principalId);
            ps.setLong(2, now);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    roleIds.add(rs.getString(1));
                    rank = Math.max(rank, rs.getInt(2));
                    if (rs.getInt(3) == 1) {
                        builtinRoleIds.add(rs.getString(1));
                    }
                }
            }
        }
        List<EffectivePolicy.Grant> allows = new ArrayList<>();
        List<EffectivePolicy.Grant> denies = new ArrayList<>();
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT rc.capability, rc.scope FROM role_capabilities rc "
                        + "JOIN role_assignments a ON a.role_id = rc.role_id "
                        + "WHERE a.principal_id = ? AND a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at > ?)")) {
            ps.setString(1, principalId);
            ps.setLong(2, now);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    addGrant(allows, denies, rs.getString(1), rs.getString(2), false);
                }
            }
        }
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT capability, scope, effect FROM principal_overrides "
                        + "WHERE principal_id = ? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?)")) {
            ps.setString(1, principalId);
            ps.setLong(2, now);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    addGrant(allows, denies, rs.getString(1), rs.getString(2), "deny".equals(rs.getString(3)));
                }
            }
        }
        return new EffectivePolicy(principalId, deviceId, rank, roleIds, builtinRoleIds, allows, denies);
    }

    /** Fail-closed mapping: unknown/malformed allows are dropped; malformed denies deny the whole capability. */
    private static void addGrant(List<EffectivePolicy.Grant> allows, List<EffectivePolicy.Grant> denies,
                                 String capabilityId, String scopeText, boolean deny) {
        Optional<Capability> capability = Capability.fromId(capabilityId);
        if (capability.isEmpty()) {
            return; // an unknown capability can never be checked, so dropping it cannot widen access
        }
        ResourceScope scope;
        try {
            scope = ResourceScope.parse(scopeText);
        } catch (StateException e) {
            if (deny) {
                denies.add(new EffectivePolicy.Grant(capability.get(), ResourceScope.ALL));
            }
            return;
        }
        if (!capability.get().scoped() && !scope.isAll()) {
            if (deny) {
                denies.add(new EffectivePolicy.Grant(capability.get(), ResourceScope.ALL));
            }
            return;
        }
        (deny ? denies : allows).add(new EffectivePolicy.Grant(capability.get(), scope));
    }

    /** Highest active role rank of a principal (0 when none/unknown). Does not require a device. */
    public int principalRank(Connection c, String principalId) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT COALESCE(MAX(r.rank), 0) FROM role_assignments a JOIN roles r ON r.id = a.role_id "
                        + "WHERE a.principal_id = ? AND a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at > ?)")) {
            ps.setString(1, principalId);
            ps.setLong(2, db.clock().millis());
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? rs.getInt(1) : 0;
            }
        }
    }

    /** Loads a role; unknown capability rows make the role unusable for delegation (fail closed). */
    public Optional<RoleInfo> role(Connection c, String roleId) throws SQLException {
        String name;
        int rank;
        boolean builtin;
        try (PreparedStatement ps = c.prepareStatement("SELECT name, rank, builtin FROM roles WHERE id = ?")) {
            ps.setString(1, roleId);
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    return Optional.empty();
                }
                name = rs.getString(1);
                rank = rs.getInt(2);
                builtin = rs.getInt(3) == 1;
            }
        }
        List<RoleGrant> grants = new ArrayList<>();
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT capability, scope FROM role_capabilities WHERE role_id = ?")) {
            ps.setString(1, roleId);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    Capability capability = Capability.fromId(rs.getString(1))
                            .orElseThrow(() -> new StateException("role_invalid", "role has an unknown capability"));
                    grants.add(new RoleGrant(capability, ResourceScope.parse(rs.getString(2))));
                }
            }
        }
        return Optional.of(new RoleInfo(roleId, name, rank, builtin, grants));
    }

    // ------------------------------------------------------------------ delegation rules (remote)

    /** Actor may act on another principal only when that principal's rank is strictly below its own. */
    public static Decision canManagePrincipal(EffectivePolicy actor, String targetPrincipalId, int targetRank) {
        if (!actor.isAuthenticated()) {
            return Decision.deny(Decision.NO_IDENTITY);
        }
        if (actor.principalId().equals(targetPrincipalId)) {
            return Decision.deny(DENY_SELF);
        }
        if (targetRank >= actor.rank()) {
            return Decision.deny(DENY_RANK);
        }
        return Decision.allow();
    }

    /** Remote role grant: delegation capability, strictly lower role and target, and no authority escalation. */
    public static Decision canGrantRole(EffectivePolicy actor, String targetPrincipalId, int targetRank, RoleInfo role) {
        Decision base = delegator(actor, Capability.ROLES_ASSIGN);
        if (!base.allowed()) {
            return base;
        }
        if (BuiltinRoles.LOCAL_ONLY_GRANT.contains(role.id())) {
            return Decision.deny(DENY_LOCAL_GRANT_ONLY);
        }
        if (role.rank() >= actor.rank()) {
            return Decision.deny(DENY_RANK);
        }
        Decision target = canManagePrincipal(actor, targetPrincipalId, targetRank);
        if (!target.allowed()) {
            return target;
        }
        return withinAuthority(actor, role.grants());
    }

    /** Remote override: allows must be within the actor's authority; denies only need rank/delegation. */
    public static Decision canSetOverride(EffectivePolicy actor, String targetPrincipalId, int targetRank,
                                          Capability capability, ResourceScope scope, boolean deny) {
        Decision base = delegator(actor, Capability.ROLES_ASSIGN);
        if (!base.allowed()) {
            return base;
        }
        if (capability.localOnly()) {
            return Decision.deny(Decision.LOCAL_ONLY);
        }
        Decision target = canManagePrincipal(actor, targetPrincipalId, targetRank);
        if (!target.allowed()) {
            return target;
        }
        return deny ? Decision.allow() : withinAuthority(actor, List.of(new RoleGrant(capability, scope)));
    }

    /** Remote revocation of a role assignment or override held by another principal. */
    public static Decision canRevokeAuthority(EffectivePolicy actor, String targetPrincipalId, int targetRank) {
        Decision base = delegator(actor, Capability.ROLES_REVOKE);
        return base.allowed() ? canManagePrincipal(actor, targetPrincipalId, targetRank) : base;
    }

    /**
     * Remote device revocation. The exact {@code devices.revoke} capability is always required first, so an
     * explicit deny wins even over the Owner's inherited allow. Then the Owner may revoke any device;
     * every other role only devices of principals strictly below its rank (not its own or equal rank).
     */
    public static Decision canRevokeDevice(EffectivePolicy actor, String devicePrincipalId, int devicePrincipalRank) {
        Decision exact = actor.check(Capability.DEVICES_REVOKE, null);
        if (!exact.allowed()) {
            return exact;
        }
        if (actor.holdsBuiltinRole(BuiltinRoles.OWNER)) {
            return Decision.allow();
        }
        return canManagePrincipal(actor, devicePrincipalId, devicePrincipalRank);
    }

    /** Remote custom-role creation: rank strictly below the actor, grants within its authority. */
    public static Decision canCreateRole(EffectivePolicy actor, int rank, List<RoleGrant> grants) {
        Decision base = delegator(actor, Capability.ROLES_CREATE);
        if (!base.allowed()) {
            return base;
        }
        if (rank < 1 || rank >= actor.rank()) {
            return Decision.deny(DENY_RANK);
        }
        return withinAuthority(actor, grants);
    }

    private static Decision delegator(EffectivePolicy actor, Capability required) {
        if (!actor.isAuthenticated()) {
            return Decision.deny(Decision.NO_IDENTITY);
        }
        return actor.check(required, null);
    }

    private static Decision withinAuthority(EffectivePolicy actor, List<RoleGrant> grants) {
        for (RoleGrant grant : grants) {
            if (grant.capability().localOnly()) {
                return Decision.deny(Decision.LOCAL_ONLY);
            }
            if (grant.capability().ownerReserved()) {
                return Decision.deny(DENY_OWNER_RESERVED);
            }
            if (grant.capability().adminReserved()) {
                return Decision.deny(DENY_ADMIN_RESERVED);
            }
            if (!actor.holdsEntirely(grant.capability(), grant.scope())) {
                return Decision.deny(DENY_NOT_DELEGABLE);
            }
        }
        return Decision.allow();
    }

    static boolean validId(String id) {
        if (id == null || id.isEmpty() || id.length() > 128) {
            return false;
        }
        for (int i = 0; i < id.length(); i++) {
            if (Character.isISOControl(id.charAt(i))) {
                return false;
            }
        }
        return true;
    }
}
