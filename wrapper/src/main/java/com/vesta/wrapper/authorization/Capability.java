package com.vesta.wrapper.authorization;

import java.util.Arrays;
import java.util.Map;
import java.util.Optional;
import java.util.function.Function;
import java.util.stream.Collectors;

/**
 * Named, exact per-action server-local capabilities. Each handler/adapter must authorize the exact
 * action it performs; there are deliberately no group or wildcard capabilities, so denying an exact
 * action (e.g. {@code lifecycle.force_kill}) can never be bypassed by a broader grant. Role presets may
 * enumerate many exact capabilities, which is the only form of "broad" default.
 *
 * <p>Persisted by {@link #id()}; unknown persisted ids are ignored by the evaluator (deny by default).
 *
 * <p>Tiers:
 * <ul>
 *   <li>{@code scoped}: takes a normalized server-relative path resource. The evaluator only matches the
 *       normalized path against scopes; actual confinement (protected internals, links, root escape)
 *       remains the adapter's job via {@link com.vesta.wrapper.files.ServerPathPolicy}.</li>
 *   <li>{@code localOnly}: never effective remotely; exercised only through LocalAuthority-proven paths.</li>
 *   <li>{@code ownerReserved}: carried only by the built-in Owner role; never delegable.</li>
 *   <li>{@code adminReserved}: carried only by Owner/Administrator built-ins; never delegable, so a
 *       lower-ranked custom role cannot smuggle administrator-equivalent security authority.</li>
 * </ul>
 */
public enum Capability {
    // ---- status and observation
    STATUS_VIEW("status.view"),
    STATUS_DETAILED("status.detailed"),
    METRICS_VIEW("metrics.view"),
    PLAYERS_VIEW("players.view"),
    ACTIVITY_VIEW("activity.view"),
    TASKS_VIEW("tasks.view"),
    DIAGNOSTICS_VIEW("diagnostics.view"),
    /** Log files, scoped by server-relative log path (e.g. {@code logs/**}). */
    LOGS_READ("logs.read", Tier.SCOPED),
    CONSOLE_READ("console.read"),
    /** Raw console: trusted-operator authority (can invoke powerful plugin commands). */
    CONSOLE_WRITE("console.write"),
    /** Typed, validated moderation actions only. */
    PLAYERS_MODERATE("players.moderate"),

    // ---- lifecycle
    LIFECYCLE_START("lifecycle.start"),
    LIFECYCLE_STOP("lifecycle.stop"),
    LIFECYCLE_RESTART("lifecycle.restart"),
    LIFECYCLE_FORCE_KILL("lifecycle.force_kill"),
    MAINTENANCE_TOGGLE("maintenance.toggle"),

    // ---- files and content
    FILES_LIST("files.list", Tier.SCOPED),
    FILES_READ("files.read", Tier.SCOPED),
    FILES_CREATE("files.create", Tier.SCOPED),
    FILES_OVERWRITE("files.overwrite", Tier.SCOPED),
    FILES_RENAME("files.rename", Tier.SCOPED),
    FILES_MKDIR("files.mkdir", Tier.SCOPED),
    FILES_DELETE("files.delete", Tier.SCOPED),
    /** Executable content install: equivalent to code execution as the game OS identity. */
    CONTENT_INSTALL("content.install"),

    // ---- backups
    BACKUPS_VIEW("backups.view"),
    BACKUPS_CREATE("backups.create"),
    BACKUPS_VERIFY("backups.verify"),
    /** Destructive restore remains owner-authorized. */
    BACKUPS_RESTORE("backups.restore", Tier.OWNER_RESERVED),
    /** Full encrypted recovery export remains owner-authorized. */
    RECOVERY_EXPORT("recovery.export", Tier.OWNER_RESERVED),

    // ---- schedules and configuration
    SCHEDULES_VIEW("schedules.view"),
    SCHEDULES_EDIT("schedules.edit"),
    SCHEDULES_RUN("schedules.run"),
    CONFIG_VIEW("config.view"),
    CONFIG_EDIT("config.edit"),
    INTEGRATIONS_EDIT("integrations.edit"),

    // ---- pack
    PACK_ACCESS("pack.access"),
    PACK_EDIT("pack.edit"),
    PACK_BUILD("pack.build"),
    PACK_PUBLISH("pack.publish"),
    PACK_FORCE_UPDATE("pack.force_update"),

    // ---- identity and delegation
    PRINCIPALS_VIEW("principals.view"),
    PRINCIPALS_MANAGE("principals.manage"),
    /**
     * Revoke devices. Always required (an explicit deny wins); a holder of the built-in Owner role may then
     * revoke any device, every other holder only devices of principals strictly below its own rank
     * (not its own or equal-rank devices).
     */
    DEVICES_REVOKE("devices.revoke"),
    /** Create custom roles strictly below the actor's rank, within its own authority. */
    ROLES_CREATE("roles.create"),
    /** Assign lower-rank roles / set overrides (and, later, invite) within the actor's own authority. */
    ROLES_ASSIGN("roles.assign"),
    /** Revoke role assignments and overrides of strictly lower-ranked principals. */
    ROLES_REVOKE("roles.revoke"),
    CREDENTIALS_MANAGE("credentials.manage", Tier.ADMIN_RESERVED),
    SECURITY_MANAGE("security.manage", Tier.ADMIN_RESERVED),
    AUDIT_VIEW("audit.view"),

    // ---- local-only hard invariants
    OWNER_RECOVERY("owner.recovery", Tier.LOCAL_ONLY),
    AGENT_INSTALL("agent.install", Tier.LOCAL_ONLY);

    private enum Tier { NORMAL, SCOPED, OWNER_RESERVED, ADMIN_RESERVED, LOCAL_ONLY }

    private static final Map<String, Capability> BY_ID = Arrays.stream(values())
            .collect(Collectors.toUnmodifiableMap(Capability::id, Function.identity()));

    private final String id;
    private final Tier tier;

    Capability(String id) {
        this(id, Tier.NORMAL);
    }

    Capability(String id, Tier tier) {
        this.id = id;
        this.tier = tier;
    }

    public String id() {
        return id;
    }

    public boolean scoped() {
        return tier == Tier.SCOPED;
    }

    public boolean localOnly() {
        return tier == Tier.LOCAL_ONLY;
    }

    public boolean ownerReserved() {
        return tier == Tier.OWNER_RESERVED;
    }

    public boolean adminReserved() {
        return tier == Tier.ADMIN_RESERVED;
    }

    /** True when remote delegation (role grant, allow override, custom role) may carry it. */
    public boolean delegable() {
        return tier == Tier.NORMAL || tier == Tier.SCOPED;
    }

    public static Optional<Capability> fromId(String id) {
        return Optional.ofNullable(id == null ? null : BY_ID.get(id));
    }
}
