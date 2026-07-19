package com.vesta.sync.auth;

import com.vesta.sync.perm.PermissionChecker;

import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.function.Predicate;

/**
 * Resolves an effective access level (0–4) and checks named capabilities.
 *
 * <p>Effective level is the maximum of anonymous/auth baselines, Minecraft OP
 * level (read directly from ops.json), UUID people grants, and configured
 * permission-node grants.
 */
public final class CapabilityEngine {

    /** @deprecated Prefer {@link AccessCapability} string names. Kept for call-site migration. */
    public enum Action {
        PUBLIC_MODPACK(AccessCapability.PACK_FETCH),
        PRIVATE_RESOURCES(AccessCapability.FILES_READ),
        MANAGE_CONTENT(AccessCapability.CONTENT_MANAGE);

        public final String capability;

        Action(String capability) {
            this.capability = capability;
        }
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

    private final AccessControlConfig config;
    private final PermissionChecker permissions;
    private final Predicate<UUID> whitelist;
    private final OpsLevelProvider opsLevels;

    public CapabilityEngine(
            AccessControlConfig config,
            PermissionChecker permissions,
            Predicate<UUID> whitelist,
            OpsLevelProvider opsLevels
    ) {
        this.config = config != null ? config : AccessControlConfig.defaults();
        this.permissions = permissions;
        this.whitelist = whitelist != null ? whitelist : new Predicate<UUID>() {
            @Override
            public boolean test(UUID uuid) {
                return false;
            }
        };
        this.opsLevels = opsLevels != null ? opsLevels : new OpsLevelProvider.None();
    }

    public CapabilityEngine(
            SecurityCapabilities capabilities,
            PermissionChecker permissions,
            Predicate<UUID> whitelist,
            OpsLevelProvider opsLevels
    ) {
        this(capabilities != null ? capabilities.access : null, permissions, whitelist, opsLevels);
    }

    public AccessControlConfig config() {
        return config;
    }

    public Decision check(Action action, AuthContext ctx) {
        return check(action.capability, ctx);
    }

    public Decision check(String capability, AuthContext ctx) {
        String required = AccessCapability.requireValid(capability);
        int level = effectiveLevel(ctx);
        if (config.capabilitiesFor(level).contains(required)) {
            return Decision.ALLOW;
        }
        if (!ctx.authenticated && couldAuthenticateFor(required, ctx)) {
            return Decision.DENY_AUTH;
        }
        if (ctx.boundUuid != null
                && ctx.mode == SecurityMode.WHITELIST
                && !whitelist.test(ctx.boundUuid)
                && config.capabilitiesFor(config.whitelistBaseline).contains(required)) {
            return Decision.DENY_WHITELIST;
        }
        return Decision.DENY;
    }

    public int effectiveLevel(AuthContext ctx) {
        int level = config.anonymousBaseline;
        if (ctx.mode == SecurityMode.OPEN) {
            level = Math.max(level, config.authenticatedBaseline);
        }
        if (ctx.authenticated) {
            level = Math.max(level, config.authenticatedBaseline);
        }
        if (ctx.passwordAuthenticated) {
            level = Math.max(level, config.passwordBaseline);
        }
        if (ctx.boundUuid != null) {
            level = Math.max(level, config.linkedBaseline);
            if (ctx.mode == SecurityMode.WHITELIST && whitelist.test(ctx.boundUuid)) {
                level = Math.max(level, config.whitelistBaseline);
            }
            Integer person = config.people.get(ctx.boundUuid);
            if (person != null) {
                level = Math.max(level, person.intValue());
            }
            level = Math.max(level, opsLevels.level(ctx.boundUuid));
            for (Map.Entry<String, Integer> entry : config.permNodes.entrySet()) {
                PermissionChecker.Result result = permissions.check(ctx.boundUuid, entry.getKey());
                if (result == PermissionChecker.Result.ALLOW) {
                    level = Math.max(level, entry.getValue().intValue());
                }
            }
        }
        if (level < 0) {
            return 0;
        }
        if (level > 4) {
            return 4;
        }
        return level;
    }

    public Set<String> capabilities(AuthContext ctx) {
        return config.capabilitiesFor(effectiveLevel(ctx));
    }

    public int minimumLevelFor(String capability) {
        String required = AccessCapability.requireValid(capability);
        for (int level = 0; level <= 4; level++) {
            if (config.capabilitiesFor(level).contains(required)) {
                return level;
            }
        }
        return 5;
    }

    public String permissionNode(Action action) {
        if (action == Action.PRIVATE_RESOURCES) {
            return firstNodeFor(AccessCapability.FILES_READ, "vesta.files.private");
        }
        if (action == Action.MANAGE_CONTENT) {
            return firstNodeFor(AccessCapability.CONTENT_MANAGE, "vesta.mods.install");
        }
        return firstNodeFor(AccessCapability.PACK_FETCH, "vesta.mods.install");
    }

    private String firstNodeFor(String capability, String fallback) {
        int needed = minimumLevelFor(capability);
        for (Map.Entry<String, Integer> entry : config.permNodes.entrySet()) {
            if (entry.getValue().intValue() >= needed) {
                return entry.getKey();
            }
        }
        return fallback;
    }

    private boolean couldAuthenticateFor(String capability, AuthContext ctx) {
        if (config.capabilitiesFor(config.authenticatedBaseline).contains(capability)) {
            return true;
        }
        if (config.capabilitiesFor(config.passwordBaseline).contains(capability)) {
            return true;
        }
        if (config.capabilitiesFor(config.linkedBaseline).contains(capability)) {
            return true;
        }
        return !config.people.isEmpty() || !config.permNodes.isEmpty();
    }
}
