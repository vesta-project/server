package com.vesta.wrapper.operations;

/**
 * Safety declarations of a handler. The runner refuses to execute an operation whose persisted
 * flags differ from the handler's declaration.
 *
 * @param externalEffectIdempotent replaying the effect after an interruption is safe
 * @param requiresConfirmation operations of this type must have passed exact confirmation
 * @param cancellableAfterEffect a cancel request may be honored at a safe point even after an
 *                               effect started (the handler leaves a consistent state there)
 */
public record HandlerContract(boolean externalEffectIdempotent, boolean requiresConfirmation,
                              boolean cancellableAfterEffect) {
}
