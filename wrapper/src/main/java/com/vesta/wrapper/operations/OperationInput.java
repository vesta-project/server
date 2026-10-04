package com.vesta.wrapper.operations;

import java.util.Locale;

/**
 * Stored canonical request input of an operation.
 *
 * @param canonicalJson the bounded canonical input, or null unless {@link InputStatus#AVAILABLE}
 * @param payloadHash SHA-256 the operation (and any confirmation) was bound to
 */
public record OperationInput(String operationId, InputStatus status, String canonicalJson, String payloadHash) {

    public enum InputStatus {
        /** Input stored and matching its bound hash. */
        AVAILABLE,
        /** No usable input (pre-v2 row, or stored input fails its hash binding). Never executed. */
        UNAVAILABLE_LEGACY,
        /** Input removed by retention after the operation finished. */
        PURGED;

        public String wireName() {
            return name().toLowerCase(Locale.ROOT);
        }

        public static InputStatus fromWire(String value) {
            return valueOf(value.toUpperCase(Locale.ROOT));
        }
    }

    @Override
    public String toString() {
        return "OperationInput[" + operationId + ", " + status + ", hash=" + payloadHash + "]";
    }
}
