package com.vesta.wrapper.operations;

/**
 * Current-authority recheck supplied by the identity/authorization layer. Called with the latest
 * operation snapshot at each {@link Stage}; a thrown exception counts as a denial (fail closed).
 * There is intentionally no allow-all implementation in this package.
 */
@FunctionalInterface
public interface OperationAuthority {

    enum Stage {
        /** After claim, before input parsing. */
        CLAIM,
        /** After parsing/preparation, immediately before the handler starts running. */
        BEFORE_RUN,
        /** Immediately before each declared external effect. */
        BEFORE_EXTERNAL_EFFECT
    }

    record Decision(boolean allowed, String reason) {
        public static Decision allow() {
            return new Decision(true, null);
        }

        public static Decision deny(String reason) {
            return new Decision(false, reason == null ? "denied" : reason);
        }
    }

    Decision recheck(Operation operation, Stage stage);
}
