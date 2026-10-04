package com.vesta.wrapper.operations;

import java.util.Set;

/**
 * A registered, typed handler for one operation type.
 *
 * <p>Contract for implementations:
 * <ul>
 *   <li>{@link #parseInput} must be side-effect free.</li>
 *   <li>Call {@link OperationContext#beginExternalEffect()} immediately before EVERY external effect
 *       (process signal, file mutation, network call). It rechecks authority and durably records
 *       that an effect may have happened before returning.</li>
 *   <li>Call {@link OperationContext#checkpoint()} only at points where stopping is safe.</li>
 *   <li>Throw {@link OperationFailure} when the outcome is known to be a failure. Any other throwable
 *       after a non-idempotent effect started is treated as an unknown outcome (recovery_required).</li>
 *   <li>Respond to thread interruption where possible; the runner cannot forcibly stop Java code.</li>
 * </ul>
 */
public interface OperationHandler<I> {

    /** Operation type this handler executes (matches {@link Operation#type()}). */
    String type();

    HandlerContract contract();

    /** Parses and validates the stored canonical input into a typed value. */
    I parseInput(String canonicalJson) throws OperationFailure;

    /** Resources the operation must hold; the claimed operation's lock set must contain all of them. */
    Set<String> requiredResources(I input);

    /** Executes the operation; returns bounded result JSON or null. */
    String execute(I input, OperationContext context) throws Exception;
}
