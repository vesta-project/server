package com.vesta.wrapper.state;

/**
 * Failure in the durable state layer. Callers must treat this as fail-closed:
 * the attempted mutation did not commit.
 */
public class StateException extends RuntimeException {
    private final String code;

    public StateException(String code, String message) {
        super(message);
        this.code = code;
    }

    public StateException(String code, String message, Throwable cause) {
        super(message, cause);
        this.code = code;
    }

    /** Stable machine-readable error code. */
    public String getCode() {
        return code;
    }
}
