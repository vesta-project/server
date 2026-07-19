package com.vesta.sync.auth;

import java.util.UUID;

/**
 * Supplies Minecraft operator permission levels (0–4) for a UUID.
 */
public interface OpsLevelProvider {
    /**
     * @return OP level 0–4, or 0 when the player is not an operator
     */
    int level(UUID uuid);

    final class None implements OpsLevelProvider {
        @Override
        public int level(UUID uuid) {
            return 0;
        }
    }
}
