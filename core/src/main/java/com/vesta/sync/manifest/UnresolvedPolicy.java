package com.vesta.sync.manifest;

/**
 * How to treat scan-path files that are not registered as Modrinth/CurseForge-known.
 */
public enum UnresolvedPolicy {
    /** List on public manifest with {@code source.type = server}; serve from live scan tree. */
    SERVER,
    /** Index as private-only; not on public manifest. */
    PRIVATE;

    public static UnresolvedPolicy parse(String raw) {
        if (raw == null || raw.trim().isEmpty()) {
            return SERVER;
        }
        String v = raw.trim().toLowerCase();
        if ("private".equals(v)) {
            return PRIVATE;
        }
        if ("server".equals(v)) {
            return SERVER;
        }
        throw new IllegalArgumentException("Unknown unresolved policy: " + raw);
    }
}
