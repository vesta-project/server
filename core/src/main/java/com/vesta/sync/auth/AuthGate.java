package com.vesta.sync.auth;

/**
 * Pure auth decision helper used by HTTP/TCP layers and tests.
 */
public final class AuthGate {

    public enum Outcome {
        ALLOW,
        DENY_MISSING,
        DENY_INVALID,
        DENY_WRONG_PASSWORD
    }

    private final SecurityMode mode;
    private final TokenService tokens;
    private final BearerTokenStore store;

    public AuthGate(SecurityMode mode, TokenService tokens, BearerTokenStore store) {
        this.mode = mode;
        this.tokens = tokens;
        this.store = store;
    }

    public SecurityMode mode() {
        return mode;
    }

    public boolean isOpen() {
        return mode == SecurityMode.OPEN;
    }

    /**
     * Read discovery endpoints (info/manifest) stay public so launchers can preview.
     * Protected routes require a bearer when mode is not OPEN.
     */
    public Outcome requireBearer(String authorizationHeaderOrToken) {
        if (mode == SecurityMode.OPEN) {
            return Outcome.ALLOW;
        }
        String token = extractBearer(authorizationHeaderOrToken);
        if (token == null || token.trim().isEmpty()) {
            return Outcome.DENY_MISSING;
        }
        return store.validate(token).isPresent() ? Outcome.ALLOW : Outcome.DENY_INVALID;
    }

    public Outcome loginWithPassword(String password) {
        if (mode == SecurityMode.OPEN) {
            return Outcome.ALLOW;
        }
        if (!tokens.hasPassword()) {
            return Outcome.DENY_INVALID;
        }
        return tokens.verifyPassword(password) ? Outcome.ALLOW : Outcome.DENY_WRONG_PASSWORD;
    }

    public String issueTokenAfterPassword(String password) {
        if (loginWithPassword(password) != Outcome.ALLOW) {
            return null;
        }
        if (mode == SecurityMode.OPEN) {
            return store.issue("anonymous", null);
        }
        return store.issue("operator", null);
    }

    public static String extractBearer(String headerOrToken) {
        if (headerOrToken == null) {
            return null;
        }
        String v = headerOrToken.trim();
        if (v.regionMatches(true, 0, "Bearer ", 0, 7)) {
            return v.substring(7).trim();
        }
        return v;
    }
}
