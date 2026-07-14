package com.vesta.sync.auth;

import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.UUID;

/**
 * Composable capability matrix from wrapper config.
 */
public final class SecurityCapabilities {

    public final CapabilityLevel publicModpack;
    public final CapabilityLevel privateResources;
    public final CapabilityLevel manageContent;
    public final Set<UUID> adminUuids;
    public final String permInstall;
    public final String permPrivateRead;
    public final boolean opGrantsManage;
    public final List<String> privatePaths;

    public SecurityCapabilities(
            CapabilityLevel publicModpack,
            CapabilityLevel privateResources,
            CapabilityLevel manageContent,
            Set<UUID> adminUuids,
            String permInstall,
            String permPrivateRead,
            boolean opGrantsManage,
            List<String> privatePaths
    ) {
        this.publicModpack = publicModpack;
        this.privateResources = privateResources;
        this.manageContent = manageContent;
        this.adminUuids = Collections.unmodifiableSet(new HashSet<>(adminUuids));
        this.permInstall = permInstall != null ? permInstall : "vesta.mods.install";
        this.permPrivateRead = permPrivateRead != null ? permPrivateRead : "vesta.files.private";
        this.opGrantsManage = opGrantsManage;
        this.privatePaths = privatePaths != null ? privatePaths : Collections.emptyList();
    }

    public static SecurityCapabilities defaults() {
        return new SecurityCapabilities(
                CapabilityLevel.OPEN,
                CapabilityLevel.DENY,
                CapabilityLevel.PERM_OR_PASSWORD,
                Collections.emptySet(),
                "vesta.mods.install",
                "vesta.files.private",
                false,
                Collections.emptyList()
        );
    }
}
