package com.vesta.wrapper.operations;

import java.util.Locale;

/** Durable operation lifecycle states. */
public enum OperationState {
    QUEUED,
    VALIDATING,
    AWAITING_CONFIRMATION,
    RUNNING,
    CANCELLING,
    SUCCEEDED,
    FAILED,
    CANCELLED,
    RECOVERY_REQUIRED;

    public String wireName() {
        return name().toLowerCase(Locale.ROOT);
    }

    public boolean terminal() {
        return this == SUCCEEDED || this == FAILED || this == CANCELLED;
    }

    /** States in which a worker holds the operation's resource locks. */
    public boolean active() {
        return this == VALIDATING || this == RUNNING || this == CANCELLING;
    }

    public static OperationState fromWire(String value) {
        for (OperationState s : values()) {
            if (s.wireName().equals(value)) {
                return s;
            }
        }
        throw new IllegalArgumentException("Unknown operation state: " + value);
    }
}
