package com.vesta.sync.auth;

/**
 * Wrapper security mode. Open allows unauthenticated access and must log a warning.
 */
public enum SecurityMode {
    OPEN,
    PASSWORD,
    LINKED,
    WHITELIST;

    public static SecurityMode parse(String raw) {
        if (raw == null || raw.trim().isEmpty()) {
            return OPEN;
        }
        String value = raw.trim().toLowerCase();
        if ("password".equals(value)) {
            return PASSWORD;
        }
        if ("linked".equals(value)) {
            return LINKED;
        }
        if ("whitelist".equals(value)) {
            return WHITELIST;
        }
        if ("open".equals(value) || "none".equals(value)) {
            return OPEN;
        }
        throw new IllegalArgumentException("Unknown security mode: " + raw);
    }

    public boolean requiresCredential() {
        return this == PASSWORD || this == LINKED || this == WHITELIST;
    }
}
