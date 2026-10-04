package com.vesta.wrapper.operations;

import com.vesta.wrapper.state.StateValues;

/** A handler-declared, known failure outcome. Message must be safe to store and show (no secrets). */
public final class OperationFailure extends Exception {
    private final String code;

    public OperationFailure(String code, String message) {
        super(StateValues.bounded(message, OperationService.MAX_ERROR_CHARS));
        this.code = StateValues.requireText("failure code", code, 64);
    }

    public String getCode() {
        return code;
    }
}
