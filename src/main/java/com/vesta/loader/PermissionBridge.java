package com.vesta.loader;

import java.util.UUID;
import java.util.function.BiFunction;

/**
 * Optional hook for loader-specific permission backends (LuckPerms / Vault / native).
 * Default is null → deny for non-ping checks.
 */
public final class PermissionBridge {

    private static volatile BiFunction<UUID, String, Boolean> delegate;

    private PermissionBridge() {}

    public static void setDelegate(BiFunction<UUID, String, Boolean> fn) {
        delegate = fn;
    }

    public static Boolean check(UUID uuid, String permission) {
        BiFunction<UUID, String, Boolean> d = delegate;
        if (d == null) {
            return null;
        }
        return d.apply(uuid, permission);
    }
}
