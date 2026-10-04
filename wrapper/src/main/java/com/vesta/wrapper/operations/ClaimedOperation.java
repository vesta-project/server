package com.vesta.wrapper.operations;

/**
 * An operation claimed by a runner. The owner token authorizes lifecycle transitions for this
 * claim only; it must never be logged, audited, emitted as an event or returned over an API.
 */
public record ClaimedOperation(Operation operation, String ownerToken) {
    @Override
    public String toString() {
        return "ClaimedOperation[" + operation.id() + ", ownerToken=<redacted>]";
    }
}
