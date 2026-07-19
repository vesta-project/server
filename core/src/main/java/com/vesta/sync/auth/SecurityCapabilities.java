package com.vesta.sync.auth;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

/**
 * Compatibility facade over {@link AccessControlConfig}.
 *
 * <p>New code should depend on {@link AccessControlConfig} directly. Legacy fields
 * approximate the old public/private/manage gates from the level table.
 */
public final class SecurityCapabilities {

    public final AccessControlConfig access;
    public final CapabilityLevel publicModpack;
    public final CapabilityLevel privateResources;
    public final CapabilityLevel manageContent;
    public final Set<UUID> adminUuids;
    public final String permInstall;
    public final String permPrivateRead;
    public final boolean opGrantsManage;
    public final List<String> privatePaths;

    public SecurityCapabilities(AccessControlConfig access) {
        this.access = access != null ? access : AccessControlConfig.defaults();
        this.publicModpack = this.access.accessRequiresAuth ? CapabilityLevel.AUTH : CapabilityLevel.OPEN;
        this.privateResources = this.access.capabilitiesFor(this.access.linkedBaseline)
                .contains(AccessCapability.FILES_READ)
                ? CapabilityLevel.LINKED
                : (this.access.capabilitiesFor(this.access.whitelistBaseline)
                .contains(AccessCapability.FILES_READ)
                ? CapabilityLevel.WHITELIST
                : CapabilityLevel.DENY);
        this.manageContent = this.access.capabilitiesFor(this.access.passwordBaseline)
                .contains(AccessCapability.CONTENT_MANAGE)
                ? CapabilityLevel.PERM_OR_PASSWORD
                : CapabilityLevel.PERM;
        this.adminUuids = this.access.people.keySet();
        this.permInstall = firstNodeAtLeast(4, "vesta.mods.install");
        this.permPrivateRead = firstNodeAtLeast(3, "vesta.files.private");
        this.opGrantsManage = true;
        this.privatePaths = this.access.privatePaths;
    }

    public static SecurityCapabilities defaults() {
        return new SecurityCapabilities(AccessControlConfig.defaults());
    }

    /**
     * Bridge for older TOML {@code [security.capabilities]} tables.
     */
    public static SecurityCapabilities fromLegacy(
            CapabilityLevel publicModpack,
            CapabilityLevel privateResources,
            CapabilityLevel manageContent,
            Set<UUID> adminUuids,
            String permInstall,
            String permPrivateRead,
            boolean opGrantsManage,
            List<String> privatePaths
    ) {
        AccessControlConfig base = AccessControlConfig.defaults();
        int anonymous = publicModpack == CapabilityLevel.OPEN ? 1 : 0;
        int authenticated = publicModpack == CapabilityLevel.DENY ? 0 : 1;
        int password = manageContent == CapabilityLevel.DENY
                ? 1
                : (manageContent == CapabilityLevel.PERM ? 1 : 4);
        int linked = privateResources == CapabilityLevel.LINKED
                || privateResources == CapabilityLevel.OPEN
                ? 3
                : 1;
        int whitelist = privateResources == CapabilityLevel.WHITELIST
                || privateResources == CapabilityLevel.LINKED
                || privateResources == CapabilityLevel.OPEN
                ? 3
                : 1;

        Map<UUID, Integer> people = new LinkedHashMap<UUID, Integer>();
        if (adminUuids != null) {
            for (UUID uuid : adminUuids) {
                people.put(uuid, Integer.valueOf(4));
            }
        }
        Map<String, Integer> nodes = new LinkedHashMap<String, Integer>();
        if (manageContent != CapabilityLevel.DENY
                && permInstall != null
                && !permInstall.trim().isEmpty()) {
            nodes.put(permInstall.trim(), Integer.valueOf(4));
        }
        if (privateResources != CapabilityLevel.DENY
                && permPrivateRead != null
                && !permPrivateRead.trim().isEmpty()) {
            nodes.put(permPrivateRead.trim(), Integer.valueOf(3));
        }
        if (opGrantsManage && manageContent != CapabilityLevel.DENY) {
            // OP levels continue to map 1:1 via OpsLevelProvider; no extra node required.
        }

        AccessControlConfig access = new AccessControlConfig(
                base.levels,
                anonymous,
                authenticated,
                password,
                linked,
                whitelist,
                people,
                nodes,
                privatePaths != null ? privatePaths : Collections.<String>emptyList()
        );
        return new SecurityCapabilities(access);
    }

    private String firstNodeAtLeast(int level, String fallback) {
        for (Map.Entry<String, Integer> entry : access.permNodes.entrySet()) {
            if (entry.getValue().intValue() >= level) {
                return entry.getKey();
            }
        }
        return fallback;
    }
}
