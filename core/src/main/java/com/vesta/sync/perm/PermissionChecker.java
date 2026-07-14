package com.vesta.sync.perm;

import java.util.UUID;

/**
 * Permission check against in-game authority (LuckPerms/Vault/native).
 */
public interface PermissionChecker {

    enum Result {
        ALLOW,
        DENY,
        UNAVAILABLE
    }

    Result check(UUID playerUuid, String permission);

    /** Always unavailable — used when mod/IPC is offline. */
    final class Unavailable implements PermissionChecker {
        @Override
        public Result check(UUID playerUuid, String permission) {
            return Result.UNAVAILABLE;
        }
    }

    /** Test double that allows everything. */
    final class AllowAll implements PermissionChecker {
        @Override
        public Result check(UUID playerUuid, String permission) {
            return Result.ALLOW;
        }
    }

    /** Test double that denies everything. */
    final class DenyAll implements PermissionChecker {
        @Override
        public Result check(UUID playerUuid, String permission) {
            return Result.DENY;
        }
    }
}
