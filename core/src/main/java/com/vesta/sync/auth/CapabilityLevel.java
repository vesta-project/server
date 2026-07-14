package com.vesta.sync.auth;

/**
 * Configurable gate for a class of API actions.
 */
public enum CapabilityLevel {
    OPEN,
    AUTH,
    LINKED,
    WHITELIST,
    DENY,
    PASSWORD,
    ADMIN_LIST,
    PERM,
    PERM_OR_PASSWORD;

    public static CapabilityLevel parse(String raw, CapabilityLevel defaultValue) {
        if (raw == null || raw.trim().isEmpty()) {
            return defaultValue;
        }
        String v = raw.trim().toLowerCase();
        switch (v) {
            case "open": return OPEN;
            case "auth": return AUTH;
            case "linked": return LINKED;
            case "whitelist": return WHITELIST;
            case "deny": return DENY;
            case "password": return PASSWORD;
            case "admin_list": return ADMIN_LIST;
            case "perm": return PERM;
            case "perm_or_password": return PERM_OR_PASSWORD;
            default:
                throw new IllegalArgumentException("Unknown capability level: " + raw);
        }
    }
}
