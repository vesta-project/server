package com.vesta.wrapper.identity;

/**
 * Failure of {@link PasswordHasher}. Carries only a fixed machine-readable code and a fixed message;
 * never any password, salt, hash or encoded-hash material.
 */
public final class PasswordHashException extends RuntimeException {
    public static final String INVALID_PASSWORD = "invalid_password";
    public static final String MALFORMED_HASH = "malformed_hash";
    public static final String UNSUPPORTED_ALGORITHM = "unsupported_algorithm";
    public static final String COST_OUT_OF_BOUNDS = "cost_out_of_bounds";
    public static final String INVALID_POLICY = "invalid_policy";

    private final String code;

    PasswordHashException(String code) {
        super("password hash error: " + code);
        this.code = code;
    }

    public String getCode() {
        return code;
    }
}
