package com.vesta.wrapper.operations;

import com.vesta.wrapper.state.StateException;

/** Operation-level rejection with a stable code. */
public final class OperationException extends StateException {
    public static final String NOT_FOUND = "not_found";
    public static final String IDEMPOTENCY_CONFLICT = "idempotency_conflict";
    public static final String INVALID_TRANSITION = "invalid_transition";
    public static final String CONFIRMATION_REJECTED = "confirmation_rejected";
    public static final String INVALID_REQUEST = "invalid_request";

    public OperationException(String code, String message) {
        super(code, message);
    }
}
