package com.vesta.sync.auth;

import com.vesta.sync.perm.PermissionChecker;

import java.util.Set;
import java.util.UUID;
import java.util.function.Predicate;

/**
 * Evaluates configured capability levels for an authenticated principal.
 */
public final class CapabilityEngine {

    public enum Action {
        PUBLIC_MODPACK,
        PRIVATE_RESOURCES,
        MANAGE_CONTENT
    }

    public enum Decision {
        ALLOW,
        DENY,
        DENY_AUTH,
        DENY_WHITELIST,
        DENY_PERM_UNAVAILABLE
    }

    public static final class AuthContext {
        public final SecurityMode mode;
        public final boolean authenticated;
        public final boolean passwordAuthenticated;
        public final UUID boundUuid;
        public final String role;

        public AuthContext(
                SecurityMode mode,
                boolean authenticated,
                boolean passwordAuthenticated,
                UUID boundUuid,
                String role
        ) {
            this.mode = mode;
            this.authenticated = authenticated;
            this.passwordAuthenticated = passwordAuthenticated;
            this.boundUuid = boundUuid;
            this.role = role;
        }
    }

    private final SecurityCapabilities caps;
    private final PermissionChecker permissions;
    private final Predicate<UUID> whitelist;
    private final Predicate<UUID> isOp;

    public CapabilityEngine(
            SecurityCapabilities caps,
            PermissionChecker permissions,
            Predicate<UUID> whitelist,
            Predicate<UUID> isOp
    ) {
        this.caps = caps;
        this.permissions = permissions;
        this.whitelist = whitelist != null ? whitelist : uuid -> false;
        this.isOp = isOp != null ? isOp : uuid -> false;
    }

    public Decision check(Action action, AuthContext ctx) {
        CapabilityLevel level = levelFor(action);
        if (level == CapabilityLevel.DENY) {
            return Decision.DENY;
        }
        if (level == CapabilityLevel.OPEN || ctx.mode == SecurityMode.OPEN) {
            return action == Action.PRIVATE_RESOURCES ? Decision.DENY : Decision.ALLOW;
        }
        switch (level) {
            case AUTH:
                return ctx.authenticated ? Decision.ALLOW : Decision.DENY_AUTH;
            case LINKED:
                return ctx.boundUuid != null ? Decision.ALLOW : Decision.DENY_AUTH;
            case WHITELIST:
                if (ctx.boundUuid == null) {
                    return Decision.DENY_AUTH;
                }
                return whitelist.test(ctx.boundUuid) ? Decision.ALLOW : Decision.DENY_WHITELIST;
            case PASSWORD:
                return ctx.passwordAuthenticated ? Decision.ALLOW : Decision.DENY_AUTH;
            case ADMIN_LIST:
                if (ctx.boundUuid != null && caps.adminUuids.contains(ctx.boundUuid)) {
                    return Decision.ALLOW;
                }
                return Decision.DENY;
            case PERM:
                return checkPerm(action, ctx, false);
            case PERM_OR_PASSWORD:
                if (ctx.passwordAuthenticated) {
                    return Decision.ALLOW;
                }
                return checkPerm(action, ctx, false);
            default:
                return Decision.DENY;
        }
    }

    private CapabilityLevel levelFor(Action action) {
        switch (action) {
            case PUBLIC_MODPACK: return caps.publicModpack;
            case PRIVATE_RESOURCES: return caps.privateResources;
            case MANAGE_CONTENT: return caps.manageContent;
            default: return CapabilityLevel.DENY;
        }
    }

    private Decision checkPerm(Action action, AuthContext ctx, boolean allowPasswordFallback) {
        if (allowPasswordFallback && ctx.passwordAuthenticated) {
            return Decision.ALLOW;
        }
        if (ctx.boundUuid == null) {
            return Decision.DENY_AUTH;
        }
        if (caps.opGrantsManage && isOp.test(ctx.boundUuid) && action == Action.MANAGE_CONTENT) {
            return Decision.ALLOW;
        }
        String node = action == Action.PRIVATE_RESOURCES ? caps.permPrivateRead : caps.permInstall;
        PermissionChecker.Result r = permissions.check(ctx.boundUuid, node);
        if (r == PermissionChecker.Result.ALLOW) {
            return Decision.ALLOW;
        }
        if (r == PermissionChecker.Result.UNAVAILABLE) {
            return Decision.DENY_PERM_UNAVAILABLE;
        }
        return Decision.DENY;
    }

    public String permissionNode(Action action) {
        return action == Action.PRIVATE_RESOURCES ? caps.permPrivateRead : caps.permInstall;
    }
}
