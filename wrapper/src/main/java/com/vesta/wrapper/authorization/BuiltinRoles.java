package com.vesta.wrapper.authorization;

import java.util.Arrays;
import java.util.EnumSet;
import java.util.List;
import java.util.Set;

import static com.vesta.wrapper.authorization.Capability.*;

/**
 * Built-in role presets. Ranks are a strict delegation hierarchy (higher = more authority); a remote
 * delegator may only act on roles and principals with a strictly lower rank. Owner and Administrator
 * can only be granted through a local, {@link com.vesta.wrapper.identity.LocalAuthority}-proven operation.
 *
 * <p>These presets are seeded by {@link com.vesta.wrapper.identity.IdentitySchema}; changing them
 * requires a new migration, not an edit here.
 */
public final class BuiltinRoles {
    public static final String OWNER = "owner";
    public static final String ADMINISTRATOR = "administrator";
    public static final String MAINTAINER = "maintainer";
    public static final String MODERATOR = "moderator";
    public static final String VIEWER = "viewer";
    public static final String PACK_USER = "pack-user";

    public static final int OWNER_RANK = 100;
    public static final int ADMINISTRATOR_RANK = 80;
    public static final int MAINTAINER_RANK = 60;
    public static final int MODERATOR_RANK = 40;
    public static final int VIEWER_RANK = 20;
    public static final int PACK_USER_RANK = 10;

    /** Roles that may only be granted locally. */
    public static final Set<String> LOCAL_ONLY_GRANT = Set.of(OWNER, ADMINISTRATOR);

    public record Preset(String id, String name, int rank, Set<Capability> capabilities) {
    }

    private BuiltinRoles() {
    }

    public static List<Preset> presets() {
        Set<Capability> ownerAll = EnumSet.noneOf(Capability.class);
        Set<Capability> adminAll = EnumSet.noneOf(Capability.class);
        for (Capability c : Capability.values()) {
            if (!c.localOnly()) {
                ownerAll.add(c);
                if (!c.ownerReserved()) {
                    adminAll.add(c);
                }
            }
        }
        return List.of(
                // Every remote capability, including owner-reserved restore/recovery export.
                new Preset(OWNER, "Owner", OWNER_RANK, Set.copyOf(ownerAll)),
                // Broad remote authority; no owner-reserved restore/recovery export, no local-only powers.
                new Preset(ADMINISTRATOR, "Administrator", ADMINISTRATOR_RANK, Set.copyOf(adminAll)),
                // Trusted operator: lifecycle (incl. force kill), raw console, files, content, backups
                // (not restore), schedules/workflows, maintenance, diagnostics, plus the read surfaces those
                // require. No roles, credentials, security policy, config, pack management, owner recovery
                // or agent installation.
                new Preset(MAINTAINER, "Maintainer", MAINTAINER_RANK, set(
                        STATUS_VIEW, STATUS_DETAILED, METRICS_VIEW, PLAYERS_VIEW, ACTIVITY_VIEW, TASKS_VIEW,
                        DIAGNOSTICS_VIEW, LOGS_READ, CONSOLE_READ, CONSOLE_WRITE,
                        LIFECYCLE_START, LIFECYCLE_STOP, LIFECYCLE_RESTART, LIFECYCLE_FORCE_KILL, MAINTENANCE_TOGGLE,
                        FILES_LIST, FILES_READ, FILES_CREATE, FILES_OVERWRITE, FILES_RENAME, FILES_MKDIR,
                        FILES_DELETE, CONTENT_INSTALL, BACKUPS_VIEW, BACKUPS_CREATE, BACKUPS_VERIFY,
                        SCHEDULES_VIEW, SCHEDULES_EDIT, SCHEDULES_RUN)),
                // Player visibility and typed moderation only: no raw console, logs or files.
                new Preset(MODERATOR, "Moderator", MODERATOR_RANK, set(
                        STATUS_VIEW, PLAYERS_VIEW, PLAYERS_MODERATE)),
                // Detailed read-only status; logs and console output are separate opt-in capabilities.
                new Preset(VIEWER, "Viewer", VIEWER_RANK, set(
                        STATUS_VIEW, STATUS_DETAILED, METRICS_VIEW, PLAYERS_VIEW, ACTIVITY_VIEW, TASKS_VIEW)),
                new Preset(PACK_USER, "Pack User", PACK_USER_RANK, set(STATUS_VIEW, PACK_ACCESS)));
    }

    private static Set<Capability> set(Capability... caps) {
        return Set.copyOf(EnumSet.copyOf(Arrays.asList(caps)));
    }
}
