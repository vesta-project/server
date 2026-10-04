package com.vesta.wrapper.state;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HexFormat;

/** Small validation and hashing helpers shared by state-layer services. */
public final class StateValues {
    private StateValues() {
    }

    /** Requires non-blank text without control characters, bounded in length. */
    public static String requireText(String name, String value, int maxLength) {
        if (value == null || value.isBlank()) {
            throw new StateException("invalid_request", name + " is required");
        }
        if (value.length() > maxLength) {
            throw new StateException("invalid_request", name + " is too long");
        }
        for (int i = 0; i < value.length(); i++) {
            if (Character.isISOControl(value.charAt(i))) {
                throw new StateException("invalid_request", name + " contains control characters");
            }
        }
        return value;
    }

    /** Like {@link #requireText} but permits null. */
    public static String optionalText(String name, String value, int maxLength) {
        return value == null ? null : requireText(name, value, maxLength);
    }

    /** Truncates free-form text (e.g. error messages) to a bound. */
    public static String bounded(String value, int maxLength) {
        if (value == null) {
            return null;
        }
        return value.length() <= maxLength ? value : value.substring(0, maxLength);
    }

    public static String sha256Hex(String value) {
        return sha256Hex(value.getBytes(StandardCharsets.UTF_8));
    }

    public static String sha256Hex(byte[] value) {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(value));
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 unavailable", e);
        }
    }
}
