package com.vesta.wrapper.authorization;

/** Result of an authorization check. {@code reason} is a stable machine-readable code. */
public record Decision(boolean allowed, String reason) {
    public static final String ALLOWED = "allowed";
    public static final String NO_IDENTITY = "no_identity";
    public static final String LOCAL_ONLY = "local_only";
    public static final String EXPLICIT_DENY = "explicit_deny";
    public static final String NOT_GRANTED = "not_granted";
    public static final String INVALID_RESOURCE = "invalid_resource";

    static Decision allow() {
        return new Decision(true, ALLOWED);
    }

    static Decision deny(String reason) {
        return new Decision(false, reason);
    }
}
